use super::*;
use std::process::Command;

/// Read a disposable fixture or test executable code directory hash. Apple silicon
/// test executables are linker-signed; bundle fixtures are explicitly ad-hoc signed.
fn cdhash(exe: &Path) -> String {
    let output = Command::new("codesign")
        .arg("-dvvv")
        .arg(exe)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stderr).into_owned();
    eprintln!("codesign -dvvv {}:\n{text}", exe.display());
    text.lines()
        .find_map(|line| line.strip_prefix("CDHash="))
        .expect("the test binary has no CDHash (unsigned)")
        .trim()
        .to_string()
}

#[test]
fn the_audit_token_names_this_process_and_only_the_right_claims_pass() {
    let (a, _b) = UnixStream::pair().unwrap();
    let exe = std::env::current_exe().unwrap();

    // The token the kernel reports is ours: audit_token_t.val[5] is the pid.
    let token = peer_audit_token(&a).unwrap();
    let pid = u32::from_ne_bytes(token[20..24].try_into().unwrap());
    assert_eq!(
        pid,
        std::process::id(),
        "audit token does not name this process"
    );

    // Positive control: the exact code directory hash and path pass.
    let hash = cdhash(&exe);
    let right = CodeRequirement::code_directory_hash(&hash).unwrap();
    SignedPeer::new(right.clone(), &exe)
        .unwrap()
        .verify(&a, PeerEnd::Server)
        .expect("positive control: the genuine code must pass");

    // A different digest is refused by the Security framework.
    let mut flipped = hash.clone().into_bytes();
    flipped[0] = if flipped[0] == b'0' { b'1' } else { b'0' };
    let wrong =
        CodeRequirement::code_directory_hash(std::str::from_utf8(&flipped).unwrap()).unwrap();
    let error = SignedPeer::new(wrong, &exe)
        .unwrap()
        .verify(&a, PeerEnd::Server)
        .unwrap_err();
    assert!(format!("{error:#}").contains(NOT_SATISFIED), "{error:#}");

    // A release policy (Apple anchor + Team ID) does not accept an ad-hoc signature.
    let release = CodeRequirement::team_and_identifier("ABCDE12345", "org.example.helper").unwrap();
    assert!(SignedPeer::new(release, &exe)
        .unwrap()
        .verify(&a, PeerEnd::Server)
        .is_err());

    // The right code at the wrong path is refused after the requirement passes.
    let error = SignedPeer::new(right, Path::new("/bin/ls"))
        .unwrap()
        .verify(&a, PeerEnd::Server)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains(DIFFERENT_EXECUTABLE),
        "{error:#}"
    );
    eprintln!("macos peer: audit token names this pid; cdhash+path accepted; wrong cdhash, release policy and wrong path refused");
}

#[test]
fn static_code_checks_the_requirement_and_outer_bundle_resource_seal() -> Result<()> {
    let root = tempfile::tempdir()?;
    let app = root.path().canonicalize()?.join("Fixture.app");
    let macos = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos)?;
    let executable = macos.join("reader");
    std::fs::copy(std::env::current_exe()?, &executable)?;
    std::fs::write(
        app.join("Contents/Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>reader</string><key>CFBundleIdentifier</key><string>org.example.fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#,
    )?;
    let resources = app.join("Contents/Resources");
    std::fs::create_dir(&resources)?;
    let resource = resources.join("policy.txt");
    std::fs::write(&resource, b"original resource")?;
    let output = Command::new("codesign")
        .args([
            "--force",
            "--sign",
            "-",
            "--identifier",
            "org.example.fixture",
        ])
        .arg(&app)
        .output()?;
    ensure!(output.status.success(), "fixture ad-hoc signing failed");
    let requirement = CodeRequirement::code_directory_hash(&cdhash(&app))?;
    verify_static_code(&app, &requirement)?;
    verify_static_code(&executable, &requirement)?;
    let wrong = CodeRequirement::team_and_identifier("ABCDE12345", "org.example.fixture")?;
    assert!(
        verify_static_code(&app, &wrong).is_err(),
        "release policy accepted ad-hoc code"
    );
    std::fs::write(&resource, b"modified resource")?;
    assert!(
        verify_static_code(&app, &requirement).is_err(),
        "modified resource seal was accepted"
    );
    std::fs::write(&resource, b"original resource")?;
    verify_static_code(&app, &requirement)?;
    assert!(verify_static_code(&root.path().join("absent"), &requirement).is_err());
    Ok(())
}

/// Real bundled peer, rather than the loose Mach-O used by the current native tests.
#[test]
fn bundled_app_peer_uses_the_main_executable() -> Result<()> {
    use crate::sidecar::supervisor::ipc::{
        code_requirement::CodeRequirement,
        macos_peer::SignedPeer,
        unix_transport::{PeerCheck, PeerEnd},
    };
    use std::{
        io::{Read, Write},
        os::unix::net::{UnixListener, UnixStream},
        process::Command,
        time::Duration,
    };
    if let Some(path) = std::env::var_os("OKILUM_TEST_BUNDLED_PEER_SOCKET") {
        let mut socket = UnixStream::connect(path)?;
        socket.write_all(b"x")?;
        let mut byte = [0];
        socket.read_exact(&mut byte)?;
        return Ok(());
    }
    struct FixtureChild(std::process::Child);
    impl Drop for FixtureChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let root = tempfile::tempdir()?;
    let app = root.path().canonicalize()?.join("Fixture.app");
    let macos = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos)?;
    let executable = macos.join("reader");
    std::fs::copy(std::env::current_exe()?, &executable)?;
    std::fs::write(
        app.join("Contents/Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>reader</string><key>CFBundleIdentifier</key><string>org.example.fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#,
    )?;
    let signed = Command::new("codesign")
        .args([
            "--force",
            "--sign",
            "-",
            "--identifier",
            "org.example.fixture",
        ])
        .arg(&app)
        .output()?;
    anyhow::ensure!(signed.status.success(), "ad-hoc fixture signing failed");
    let details = Command::new("codesign").arg("-dvvv").arg(&app).output()?;
    let details = String::from_utf8(details.stderr)?;
    let digest = details
        .lines()
        .find_map(|s| s.strip_prefix("CDHash="))
        .context("fixture hash")?;
    let requirement = CodeRequirement::code_directory_hash(digest)?;
    let listener = UnixListener::bind(root.path().join("peer.sock"))?;
    let mut child = FixtureChild(
        Command::new(&executable)
            .args([
                "--exact",
                "sidecar::supervisor::ipc::macos_peer::tests::bundled_app_peer_uses_the_main_executable",
                "--nocapture",
            ])
            .env("OKILUM_TEST_BUNDLED_PEER_SOCKET", root.path().join("peer.sock"))
            .spawn()?,
    );
    listener.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let (mut socket, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                anyhow::ensure!(
                    std::time::Instant::now() < deadline && child.0.try_wait()?.is_none(),
                    "bundled fixture failed to connect"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    };
    socket.set_read_timeout(Some(Duration::from_secs(20)))?;
    let mut byte = [0];
    socket.read_exact(&mut byte)?;
    let positive =
        SignedPeer::new(requirement.clone(), &executable)?.verify(&socket, PeerEnd::Client);
    println!("REAL_BUNDLED_PEER_RESULT {positive:?}");
    anyhow::ensure!(
        SignedPeer::new(requirement.clone(), &app)?
            .verify(&socket, PeerEnd::Client)
            .is_err(),
        "bundle directory accepted as an executable"
    );
    let wrong_path = SignedPeer::new(requirement, std::path::Path::new("/bin/ls"))?
        .verify(&socket, PeerEnd::Client);
    anyhow::ensure!(wrong_path.is_err(), "wrong executable accepted");
    let wrong = CodeRequirement::code_directory_hash("0000000000000000000000000000000000000000")?;
    anyhow::ensure!(
        SignedPeer::new(wrong, &executable)?
            .verify(&socket, PeerEnd::Client)
            .is_err(),
        "wrong digest accepted"
    );
    socket.write_all(b"q")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.0.try_wait()? {
            anyhow::ensure!(status.success(), "bundled child did not finish its test");
            break;
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "bundled child did not exit"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    positive.context("real bundled peer must authenticate as its executable")?;
    Ok(())
}
