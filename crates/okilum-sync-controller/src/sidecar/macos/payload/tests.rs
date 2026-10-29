use super::*;
use sha2::{Digest, Sha256};
use std::{
    net::{TcpListener, TcpStream},
    os::unix::fs::{symlink, PermissionsExt},
};
use uuid::Uuid;

struct Fixture {
    _root: tempfile::TempDir,
    state: PathBuf,
    binding: Binding,
    identity: DaemonIdentity,
    selection: Selection,
    listener: TcpListener,
}
impl Fixture {
    fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let state = root.path().canonicalize()?;
        // Tempdir uses the host umask; explicitly prepare the private state root,
        // just as the real preparation layer must, on both Unix platforms.
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
        assert_eq!(fs::metadata(&state)?.mode() & 0o7777, 0o700);
        for relative in ["config", "data", "runtime", "runtime/2.1.6"] {
            let path = state.join(relative);
            fs::create_dir(&path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let config_file = state.join("config/config.xml");
        fs::write(&config_file, format!("<configuration><gui tls=\"false\"><address>{}</address><apikey>synthetic-key</apikey></gui></configuration>", listener.local_addr()?))?;
        fs::write(state.join("config/key.pem"), b"synthetic private fixture")?;
        fs::write(state.join("config/cert.pem"), b"synthetic certificate")?;
        for path in [&config_file, &state.join("config/key.pem")] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        let runtime = state.join("runtime/2.1.6/syncthing");
        fs::write(&runtime, b"synthetic runtime, never executed")?;
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755))?;
        let identity = DaemonIdentity {
            config_file,
            device_id: "existing-fixture-device".into(),
            certificate_hash: format!("{:x}", Sha256::digest(b"synthetic certificate")),
        };
        let binding = Binding {
            instance: Uuid::new_v4(),
            installation: Uuid::new_v4(),
            owner: rustix::process::geteuid().as_raw().to_string(),
            supervisor: "/Applications/Fixture.app/Contents/MacOS/okilum-sync-supervisor".into(),
            state_directory: state.to_str().unwrap().into(),
            device_identity: identity.device_id.clone(),
        };
        let selection = Selection::new(
            "2.1.6",
            &format!("{:x}", Sha256::digest(b"synthetic runtime, never executed")),
            &runtime,
        )?;
        Ok(Self {
            _root: root,
            state,
            binding,
            identity,
            selection,
            listener,
        })
    }
    fn payload(&self) -> Payload {
        Payload::new(
            self.binding.clone(),
            self.selection.clone(),
            self.identity.clone(),
        )
    }
}

#[test]
fn offline_payload_verification_builds_fixed_launch_paths_without_contacting_rest() -> Result<()> {
    let f = Fixture::new()?;
    let before = fs::read(f.state.join("config/config.xml"))?;
    let launch = f.payload().verify(&f.binding)?;
    assert_eq!(Path::new(&launch.config), f.state.join("config"));
    assert_eq!(Path::new(&launch.data), f.state.join("data"));
    assert_eq!(
        Path::new(&launch.executable),
        f.state.join("runtime/2.1.6/syncthing")
    );
    assert!(launch
        .unix_arguments()?
        .contains(&"--no-upgrade".to_owned()));
    assert_eq!(fs::read(f.state.join("config/config.xml"))?, before);
    assert!(f
        .listener
        .accept()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::WouldBlock));
    // Positive control proves the listener would detect any connection.
    let _connection = TcpStream::connect(f.listener.local_addr()?)?;
    // Connection completion and accept readiness need not be observed in the
    // same scheduling instant on every kernel. Await this one positive-control
    // connection, preserving all errors except readiness/interruption.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        match f.listener.accept() {
            Ok(_) => break,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                ensure!(
                    std::time::Instant::now() < deadline,
                    "listener positive control did not become ready: {error}"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[test]
fn changed_runtime_or_certificate_is_refused_and_originals_remain_usable() -> Result<()> {
    let f = Fixture::new()?;
    let payload = f.payload();
    let runtime = f.selection.resolve(&f.state)?;
    fs::write(&runtime, b"changed runtime")?;
    assert!(payload.verify(&f.binding).is_err());
    fs::write(&runtime, b"synthetic runtime, never executed")?;
    let certificate = f.state.join("config/cert.pem");
    fs::write(&certificate, b"changed certificate")?;
    assert!(payload.verify(&f.binding).is_err());
    fs::write(certificate, b"synthetic certificate")?;
    payload.verify(&f.binding)?;
    Ok(())
}

#[test]
fn linked_ancestors_and_shared_credentials_are_not_adopted() -> Result<()> {
    let f = Fixture::new()?;
    let payload = f.payload();
    let key = f.state.join("config/key.pem");
    fs::set_permissions(&key, fs::Permissions::from_mode(0o644))?;
    assert!(payload.verify(&f.binding).is_err());
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600))?;
    let runtime = f.state.join("runtime/2.1.6");
    let renamed = f.state.join("original-runtime");
    fs::rename(&runtime, &renamed)?;
    symlink(&renamed, &runtime)?;
    assert!(
        payload.verify(&f.binding).is_err(),
        "linked parent bypassed the file digest"
    );
    fs::remove_file(&runtime)?;
    fs::rename(renamed, runtime)?;
    payload.verify(&f.binding)?;
    Ok(())
}

#[test]
fn receipt_binding_and_supported_version_cannot_be_substituted() -> Result<()> {
    let f = Fixture::new()?;
    let payload = f.payload();
    let mut binding = f.binding.clone();
    binding.device_identity.push('x');
    assert!(payload.verify(&binding).is_err());
    let runtime = f.selection.resolve(&f.state)?;
    for version in ["2.1.7", "vv2.1.6", "1.29.5"] {
        let selection = Selection::new(version, f.selection.digest(), &runtime)?;
        let error = Payload::new(f.binding.clone(), selection, f.identity.clone())
            .verify(&f.binding)
            .err()
            .unwrap();
        assert!(error
            .to_string()
            .contains("unsupported prepared Syncthing version"));
    }
    let mut identity = f.identity.clone();
    identity.config_file = f.state.join("other.xml");
    assert!(
        Payload::new(f.binding.clone(), f.selection.clone(), identity)
            .verify(&f.binding)
            .is_err()
    );
    payload.verify(&f.binding)?;
    Ok(())
}

#[test]
fn special_permission_bits_on_payload_files_and_directories_are_refused() -> Result<()> {
    let f = Fixture::new()?;
    let payload = f.payload();
    let mut paths = vec![
        f.state.join("config/config.xml"),
        f.state.join("config/key.pem"),
        f.state.join("config/cert.pem"),
        f.selection.resolve(&f.state)?,
        f.state.join("data"),
    ];
    for path in paths.drain(..) {
        let original = fs::metadata(&path)?.permissions();
        for special in [0o4000, 0o2000, 0o1000] {
            fs::set_permissions(&path, fs::Permissions::from_mode(original.mode() | special))?;
            assert_eq!(
                fs::metadata(&path)?.mode() & special,
                special,
                "positive control: filesystem did not retain the test permission bit"
            );
            assert!(payload.verify(&f.binding).is_err());
        }
        fs::set_permissions(path, original)?;
    }
    payload.verify(&f.binding)?;
    Ok(())
}

#[test]
fn registration_retains_the_exact_durable_selection_receipt() -> Result<()> {
    let f = Fixture::new()?;
    f.payload().verify_selected(&f.binding, &f.selection)?; // positive control
    let changed = Selection::new("2.1.6", &"0".repeat(64), &f.selection.resolve(&f.state)?)?;
    assert!(f.payload().verify_selected(&f.binding, &changed).is_err());
    Ok(())
}
