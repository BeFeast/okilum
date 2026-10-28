//! The macOS-facing daemon API uses the same authenticated inspection as Linux.
//! Only a disposable loopback HTTP fixture is used; no Syncthing process is run.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use anyhow::Result;
use okilum_sync_controller::{
    daemon::{discover, DaemonIdentity, CLIENT_VERSION},
    folder::FolderController,
};
use serde_json::json;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const DEVICE: &str = "AAAAAAA-AAAAAAA-AAAAAAA-AAAAAAA-AAAAAAA-AAAAAAA-AAAAAAA-AAAAAAA";
const KEY: &str = "synthetic-fixture-key";

struct Fixture {
    config: PathBuf,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    server: Option<JoinHandle<()>>,
}
impl Fixture {
    fn new(root: &Path, name: &str, device: &str, version: &str) -> Result<Self> {
        let home = root.join(name);
        fs::create_dir(&home)?;
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
        let config = home.join("config.xml");
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        fs::write(
            &config,
            format!(
                "<configuration><gui tls=\"false\"><address>{}</address><apikey>{KEY}</apikey></gui></configuration>",
                listener.local_addr()?
            ),
        )?;
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600))?;
        fs::write(home.join("cert.pem"), b"synthetic certificate")?;
        let requests = Arc::new(Mutex::new(vec![]));
        let stop = Arc::new(AtomicBool::new(false));
        let (recorded, stopping) = (requests.clone(), stop.clone());
        let (device, version) = (device.to_owned(), version.to_owned());
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !stopping.load(Ordering::SeqCst) && Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request = line.trim().to_string();
                let mut authenticated = false;
                let mut bytes = line.len();
                loop {
                    line.clear();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    bytes += line.len();
                    assert!(bytes <= 8192, "fixture request too large");
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("x-api-key") {
                            authenticated = value.trim() == KEY;
                        }
                    }
                }
                assert!(authenticated, "inspection omitted authentication");
                recorded.lock().unwrap().push(request.clone());
                let body = match request.as_str() {
                    "GET /rest/system/version HTTP/1.1" => json!({"version":version}),
                    "GET /rest/system/status HTTP/1.1" => json!({"myID":device}),
                    "GET /rest/config HTTP/1.1" => json!({"folders":[], "devices":[]}),
                    _ => panic!("unexpected or mutating REST call: {request}"),
                }
                .to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        Ok(Self {
            config: config.canonicalize()?,
            requests,
            stop,
            server: Some(server),
        })
    }
    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    fn identity(&self) -> DaemonIdentity {
        discover(std::slice::from_ref(&self.config))
            .select(&self.config)
            .unwrap()
            .identity
            .clone()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(server) = self.server.take() {
            let result = server.join();
            if !thread::panicking() {
                result.unwrap();
            }
        }
    }
}

#[test]
fn saved_identity_rechecks_certificate_device_and_version() -> Result<()> {
    let root = tempfile::tempdir()?;
    let fixture = Fixture::new(root.path(), "valid", DEVICE, CLIENT_VERSION)?;
    let identity = fixture.identity();
    identity.connect()?;
    assert_eq!(fixture.calls(), 5, "discovery plus authenticated connect");
    fs::write(fixture.config.with_file_name("cert.pem"), b"replacement")?;
    let before = fixture.calls();
    assert!(identity.connect().is_err());
    assert_eq!(fixture.calls(), before, "changed certificate reached REST");

    let changed = Fixture::new(root.path(), "changed", "another-device", CLIENT_VERSION)?;
    let mut identity = changed.identity();
    identity.device_id = DEVICE.to_owned();
    let before = changed.calls();
    assert!(identity.connect().is_err());
    assert_eq!(changed.calls(), before + 1, "wrong device queried version");

    let old = Fixture::new(root.path(), "old", DEVICE, "v1.29.5")?;
    // The hub version may be inventoried for overlap, but cannot become a client.
    let identity = old.identity();
    assert!(identity.connect().is_err());
    assert_eq!(old.calls(), 5);
    Ok(())
}

#[test]
fn incomplete_inventory_blocks_selection_and_never_assumes_an_absence() -> Result<()> {
    let root = tempfile::tempdir()?;
    let fixture = Fixture::new(root.path(), "valid", DEVICE, CLIENT_VERSION)?;
    let absent = root.path().join("missing.xml");
    let inventory = discover(&[fixture.config.clone(), absent.clone()]);
    assert_eq!(inventory.candidates.len(), 1, "positive inventory control");
    assert_eq!(inventory.unavailable, vec![absent]);
    assert!(inventory.select(&fixture.config).is_err());
    assert!(inventory.folders().is_err());
    let alias = root.path().join("config-alias.xml");
    symlink(&fixture.config, &alias)?;
    let inventory = discover(&[fixture.config.clone(), alias]);
    assert!(inventory.unavailable.is_empty());
    assert_eq!(inventory.candidates.len(), 1);
    inventory.select(&fixture.config)?;
    Ok(())
}

#[test]
fn unsupported_or_nonloopback_configs_fail_without_network_or_folder_state() -> Result<()> {
    let root = tempfile::tempdir()?;
    let fixture = Fixture::new(root.path(), "valid", DEVICE, CLIENT_VERSION)?;
    let identity = fixture.identity();
    let before = fixture.calls();
    for address in ["192.0.2.1:8384", "localhost:8384", "127.0.0.1:0"] {
        fs::write(&fixture.config, format!("<configuration><gui><address>{address}</address><apikey>{KEY}</apikey></gui></configuration>"))?;
        assert!(identity.connect().is_err());
        let inventory = discover(std::slice::from_ref(&fixture.config));
        assert_eq!(inventory.unavailable.len(), 1);
        assert_eq!(fixture.calls(), before);
    }
    let state = root.path().join("folder-state");
    let folder = FolderController::new(state.clone());
    assert!(!folder.exists()?);
    folder.request_remove()?;
    assert!(!state.exists(), "an unenabled folder prepared state");
    Ok(())
}
