//! Disposable real protocol receiver for signed-beta Mac UI acceptance.
#![cfg(target_os = "macos")]
use anyhow::{Context, Result};
use okilum_sync_controller::direct::Controller;
use serde_json::{json, Value};
use std::{io::{self, BufRead, Write}, path::PathBuf};
#[test]
fn native_peer() -> Result<()> {
    let state = PathBuf::from(std::env::var_os("OKILUM_QA_PEER_STATE").context("peer state")?);
    let config = PathBuf::from(std::env::var_os("OKILUM_QA_PEER_CONFIG").context("peer config")?);
    let controller = Controller::open(state, vec![config])?;
    println!("PEER {}", json!({"ready":true})); io::stdout().flush()?;
    for line in io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line?)?;
        let result = (|| -> Result<Value> {
            Ok(match request["op"].as_str().context("op")? {
                "redeem" => serde_json::to_value(controller.redeem(request["url"].as_str().context("url")?, "Disposable headless peer")?)?,
                "refresh" => serde_json::to_value(controller.refresh()?)?,
                "catalog" => serde_json::to_value(controller.catalog(request["peer"].as_str().context("peer")?.parse()?)?)?,
                "attach" => serde_json::to_value(controller.attach(request["peer"].as_str().context("peer")?.parse()?, request["vault"].as_str().context("vault")?, &PathBuf::from(request["path"].as_str().context("path")?))?)?,
                _ => anyhow::bail!("unknown operation"),
            })
        })();
        let reply = match result { Ok(value) => json!({"ok":value}), Err(error) => json!({"error":format!("{error:#}")}) };
        println!("PEER {}", reply); io::stdout().flush()?;
    }
    controller.shutdown(); Ok(())
}

/// Real bundled peer, rather than the loose Mach-O used by the current native tests.
#[test]
fn bundled_peer_executable_identity() -> Result<()> {
    use okilum_sync_controller::sidecar::supervisor::ipc::{
        code_requirement::CodeRequirement,
        macos_peer::SignedPeer,
        unix_transport::{PeerCheck, PeerEnd},
    };
    use std::{io::{Read, Write}, os::unix::net::{UnixListener, UnixStream}, process::Command, time::Duration};
    if let Some(path) = std::env::var_os("OKILUM_QA_BUNDLE_CHILD") {
        let mut socket = UnixStream::connect(path)?;
        socket.write_all(b"x")?;
        let mut byte=[0]; socket.read_exact(&mut byte)?;
        return Ok(());
    }
    let root=tempfile::tempdir()?;
    let app=root.path().canonicalize()?.join("Fixture.app");
    let macos=app.join("Contents/MacOS"); std::fs::create_dir_all(&macos)?;
    let executable=macos.join("reader"); std::fs::copy(std::env::current_exe()?,&executable)?;
    std::fs::write(app.join("Contents/Info.plist"),r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>reader</string><key>CFBundleIdentifier</key><string>org.example.fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#)?;
    let signed=Command::new("codesign").args(["--force","--sign","-","--identifier","org.example.fixture"]).arg(&app).output()?;
    anyhow::ensure!(signed.status.success(),"ad-hoc fixture signing failed");
    let details=Command::new("codesign").arg("-dvvv").arg(&app).output()?;
    let details=String::from_utf8(details.stderr)?;
    let digest=details.lines().find_map(|s|s.strip_prefix("CDHash=")).context("fixture hash")?;
    let requirement=CodeRequirement::code_directory_hash(digest)?;
    let listener=UnixListener::bind(root.path().join("peer.sock"))?;
    let mut child=Command::new(&executable).args(["--exact","bundled_peer_executable_identity","--nocapture"]).env("OKILUM_QA_BUNDLE_CHILD",root.path().join("peer.sock")).spawn()?;
    let outcome=(|| -> Result<()> {
        let (mut socket,_)=listener.accept()?;
        socket.set_read_timeout(Some(Duration::from_secs(20)))?;
        let mut byte=[0]; socket.read_exact(&mut byte)?;
        let positive=SignedPeer::new(requirement.clone(),&executable)?.verify(&socket,PeerEnd::Client);
        println!("REAL_BUNDLED_PEER_RESULT {positive:?}");
        let wrong_path=SignedPeer::new(requirement, std::path::Path::new("/bin/ls"))?.verify(&socket,PeerEnd::Client);
        anyhow::ensure!(wrong_path.is_err(),"wrong executable accepted");
        let wrong=CodeRequirement::code_directory_hash("0000000000000000000000000000000000000000")?;
        anyhow::ensure!(SignedPeer::new(wrong,&executable)?.verify(&socket,PeerEnd::Client).is_err(),"wrong digest accepted");
        socket.write_all(b"q")?;
        positive.context("real bundled peer must authenticate as its executable")?;
        Ok(())
    })();
    let _=child.kill();let _=child.wait();
    outcome
}
