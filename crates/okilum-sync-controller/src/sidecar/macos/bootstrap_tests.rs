//! Isolated offline composition fixtures, not native daemon/service acceptance.
use super::*;
use std::os::unix::fs::PermissionsExt;

fn fixture(root: &Path) -> Result<(PathBuf, String)> {
    let runtime = root.join("fixture-runtime");
    let id = ["AAAAAAA"; 8].join("-");
    let source = format!(
        r##"#!/bin/sh
case "$1" in
--version) echo 'syncthing v2.1.6 fixture' ;;
generate)
    test "$2" = --home || exit 2
    test ! -e "$3/fixture-fired" || exit 3
    /bin/echo fired > "$3/fixture-fired"
    /bin/cat > "$3/config.xml" <<'XML'
<configuration><folder id="default" path="/must-not-sync"/><gui tls="false"><address>127.0.0.1:8384</address><apikey>isolated-fixture-key</apikey></gui><options><listenAddress>default</listenAddress></options></configuration>
XML
    /bin/echo fixture-certificate > "$3/cert.pem"
    /bin/echo fixture-private-key > "$3/key.pem"
    ;;
device-id) echo '{id}' ;;
*) exit 2 ;;
esac
"##
    );
    fs::write(&runtime, source)?;
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o500))?;
    let digest = format!("{:x}", Sha256::digest(fs::read(&runtime)?));
    Ok((runtime, digest))
}
fn state(root: &Path, name: &str) -> Result<PathBuf> {
    let state = root.join(name);
    fs::create_dir(&state)?;
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
    Ok(state)
}
#[test]
fn offline_preparation_stages_exact_bytes_and_creates_private_lan_config() -> Result<()> {
    let root = tempfile::tempdir()?;
    let root = root.path().canonicalize()?;
    let (source, digest) = fixture(&root)?;
    let state = state(&root, "state")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let identity = prepare_payload(&state, &source, &digest, b"fixture provenance", deadline)?;
    assert!(
        state.join("config/fixture-fired").is_file(),
        "generator positive control"
    );
    let selection = UnixStore::open_existing(&state)?
        .read_selection(deadline)?
        .unwrap();
    assert_eq!(fs::read(selection.verify_file(&state)?)?, fs::read(source)?);
    identity.verify_local()?;
    let xml = xmltree::Element::parse(private::read(&identity.config_file)?.as_slice())?;
    assert!(xml.get_child("folder").is_none());
    let options = xml.get_child("options").unwrap();
    assert_eq!(
        options
            .get_child("globalAnnounceEnabled")
            .unwrap()
            .get_text()
            .as_deref(),
        Some("false")
    );
    assert_eq!(
        options
            .get_child("localAnnounceEnabled")
            .unwrap()
            .get_text()
            .as_deref(),
        Some("true")
    );
    assert!(
        prepare_payload(
            &state,
            &root.join("fixture-runtime"),
            &digest,
            b"fixture provenance",
            deadline
        )
        .is_err(),
        "never replace generated identity"
    );
    Ok(())
}
#[test]
fn changed_runtime_refuses_before_offline_generation_or_selection() -> Result<()> {
    let root = tempfile::tempdir()?;
    let root = root.path().canonicalize()?;
    let (source, _) = fixture(&root)?;
    let state = state(&root, "refused")?;
    assert!(prepare_payload(
        &state,
        &source,
        &"0".repeat(64),
        b"fixture",
        Instant::now() + Duration::from_secs(3)
    )
    .is_err());
    assert!(!state.join("config/fixture-fired").exists());
    assert!(UnixStore::open_existing(&state)?
        .read_selection(Instant::now() + Duration::from_secs(1))?
        .is_none());
    Ok(())
}
#[test]
fn no_opt_in_cannot_reach_inventory_state_or_bundle() {
    let requirement =
        CodeRequirement::team_and_identifier("ABCDE12345", "org.example.fixture").unwrap();
    let policy = SigningPolicy {
        app: requirement.clone(),
        helper: requirement.clone(),
        runtime: requirement,
    };
    let error = connect_current_app(false, policy, Duration::from_secs(1)).unwrap_err();
    assert!(error.to_string().contains("explicit Connect consent"));
}

#[test]
fn ready_readback_requires_exact_identity_and_rejects_redirects() -> Result<()> {
    use std::io::{Read, Write};
    for (wrong_identity, redirect, expected_requests) in
        [(false, false, 2), (true, false, 1), (false, true, 1)]
    {
        let root = tempfile::tempdir()?;
        let root = root.path().canonicalize()?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let identity = DaemonIdentity {
            config_file: root.join("config.xml"),
            device_id: ["AAAAAAA"; 8].join("-"),
            certificate_hash: format!("{:x}", Sha256::digest(b"fixture-cert")),
        };
        private::write(&root.join("cert.pem"), b"fixture-cert")?;
        private::write(&identity.config_file,format!("<configuration><gui tls=\"false\"><address>{address}</address><apikey>fixture-private-key</apikey></gui></configuration>").as_bytes())?;
        let id = identity.device_id.clone();
        let server = std::thread::spawn(move || -> Result<()> {
            listener.set_nonblocking(true)?;
            let deadline = Instant::now() + Duration::from_secs(3);
            for request in 0..expected_requests {
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            ensure!(Instant::now() < deadline, "fixture request missing");
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(e) => return Err(e.into()),
                    }
                };
                socket.set_read_timeout(Some(Duration::from_secs(1)))?;
                let mut bytes = [0u8; 8192];
                let count = socket.read(&mut bytes)?;
                let request_text = std::str::from_utf8(&bytes[..count])?.to_ascii_lowercase();
                ensure!(
                    request_text.contains("x-api-key: fixture-private-key"),
                    "fixture did not observe authenticated request"
                );
                let response = if redirect {
                    "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
                } else {
                    let body = if request == 0 {
                        serde_json::json!({"myID":if wrong_identity {"wrong-device"} else {&id}})
                    } else {
                        serde_json::json!({"version":CLIENT_VERSION})
                    }
                    .to_string();
                    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len())
                };
                socket.write_all(response.as_bytes())?;
            }
            Ok(())
        });
        let result = verify_rest(&identity, Instant::now() + Duration::from_secs(2));
        assert_eq!(result.is_ok(), !wrong_identity && !redirect);
        server.join().unwrap()?;
    }
    Ok(())
}
