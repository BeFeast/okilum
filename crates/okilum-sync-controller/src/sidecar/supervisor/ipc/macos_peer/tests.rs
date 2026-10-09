use super::*;
use std::process::Command;

/// The code directory hash codesign reports for the running test binary. On Apple
/// silicon the linker ad-hoc signs every binary, so this exists on the CI runner.
fn own_cdhash() -> String {
    let exe = std::env::current_exe().unwrap();
    let output = Command::new("codesign")
        .arg("-dvvv")
        .arg(&exe)
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
    let hash = own_cdhash();
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
