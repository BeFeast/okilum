"""Diagnostic-only hosted controls: fail with old Mac cookies, then pass restored."""
import pathlib
import subprocess


def run(*args, check=True):
    return subprocess.run(args, text=True, check=check, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)


def tests(package, target, filter, expected):
    command = ["cargo", "test", "--release", "--locked", "--target", "aarch64-apple-darwin", "--workspace", "-p", package, target, filter, "--", "--test-threads=1", "--nocapture"]
    result = run(*command, check=False)
    print(result.stdout, flush=True)
    assert (result.returncode == 0) == expected, (filter, result.returncode, expected)
    assert "test result:" in result.stdout, "compile/setup error is not a negative verdict"
    assert ("0 failed" if expected else "FAILED") in result.stdout


shell = pathlib.Path("crates/okilum-shell/src/reader_project_navigation.rs")
store = pathlib.Path("crates/okilum-shell/src/reader_manager_store.rs")
bridge = pathlib.Path("crates/okilum-shell/src/reader_manager_focus/bridge.m")
session = pathlib.Path("crates/okilum-t3/src/session.rs")
native = pathlib.Path("crates/okilum-shell/src/project/native.rs")
originals = {path: path.read_bytes() for path in [shell, store, bridge, session, native]}
try:
    # Recover the actual baseline Mac bridge and cookie source, not an unfired stub.
    baseline = "e96acbdc82c71fd143474bbf20dec1e9b6845dcc"
    run("git", "fetch", "--depth=1", "origin", baseline)
    for path in [store, bridge]:
        path.write_text(run("git", "show", f"{baseline}:{path}").stdout)
    text = shell.read_text()
    old = "let session = okilum_t3::SessionGateway::native(env, native)?.with_access(access);"
    assert text.count(old) == 1
    shell.write_text(text.replace(old, "let _ = native;\n    let session = okilum_t3::SessionGateway::new(env, Arc::new(crate::reader_manager_store::ManagerCookies))?.with_access(access);"))
    # Verify mutation compiles; failure must come from the assertion, not rustc.
    run("cargo", "test", "--release", "--locked", "--target", "aarch64-apple-darwin", "--workspace", "--no-run")
    print("NEGATIVE: actual pre-fix macOS cookie gateway", flush=True)
    tests("okilum-shell", "--bins", "reader_project_navigation::token_tests::", False)
    for path in [shell, store, bridge]:
        path.write_bytes(originals[path])
    # A populated cookie-source spy must also detect accidental cookie discovery.
    text = session.read_text()
    assert text.count("gateway.native = Some(token);") == 1
    session.write_text(text.replace("gateway.native = Some(token);", "let _ = token; gateway.native = None;"))
    run("cargo", "test", "--release", "--locked", "--target", "aarch64-apple-darwin", "--workspace", "--no-run")
    print("NEGATIVE: native constructor incorrectly selects cookie adapter", flush=True)
    tests("okilum-t3", "--lib", "session::tests::native_", False)
    session.write_bytes(originals[session])
    text = native.read_text()
    assert text.count("self.gateway = Some(gateway);") == 1
    native.write_text(text.replace("self.gateway = Some(gateway);", "let _ = gateway;"))
    run("cargo", "test", "--release", "--locked", "--target", "aarch64-apple-darwin", "--workspace", "--no-run")
    print("NEGATIVE: newly paired client leaves Board on the unpaired gateway", flush=True)
    tests("okilum-shell", "--bins", "native_pairing_immediately_replaces_the_boards_unpaired_gateway_2520", False)
finally:
    for path, content in originals.items():
        path.write_bytes(content)
print("RESTORED: exact parent production source", flush=True)
tests("okilum-shell", "--bins", "reader_project_navigation::token_tests::", True)
tests("okilum-t3", "--lib", "session::tests::native_", True)
tests("okilum-shell", "--bins", "native_pairing_immediately_replaces_the_boards_unpaired_gateway_2520", True)
assert not run("git", "diff", "--", *map(str, originals)).stdout
print("PASS: all three negative controls fail and restored production passes with a clean diff", flush=True)
