"""Hosted retry: finish the post-pairing control; prior cookie controls are recorded."""
from pathlib import Path
import subprocess

FILTER = "native_pairing_immediately_replaces_the_boards_unpaired_gateway_2520"
NATIVE = Path("crates/okilum-shell/src/project/native.rs")
CARGO = ["cargo", "test", "--release", "--locked", "--target", "aarch64-apple-darwin", "--workspace"]


def run(args):
    result = subprocess.run(args, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    if result.returncode:
        print(result.stdout, flush=True)
        result.check_returncode()
    print("\n".join(result.stdout.splitlines()[-10:]), flush=True)
    return result


def test(expected):
    result = subprocess.run([*CARGO, "--bins", FILTER, "--", "--test-threads=1", "--nocapture"],
        text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    print(result.stdout, flush=True)
    assert (result.returncode == 0) == expected
    assert FILTER + (" ... ok" if expected else " ... FAILED") in result.stdout
    assert ("1 passed; 0 failed" if expected else "0 passed; 1 failed") in result.stdout


original = NATIVE.read_bytes()
try:
    source = original.decode()
    assert source.count("self.gateway = Some(gateway);") == 1
    NATIVE.write_text(source.replace("self.gateway = Some(gateway);", "let _ = gateway;"))
    print("MUTATION: paired client leaves the current Board on its unpaired gateway; compiling", flush=True)
    # A compiler error is never accepted as a failing regression.
    run([*CARGO, "--no-run"])
    print("NEGATIVE: post-pairing Board refresh removed; mutant compiled", flush=True)
    test(False)
finally:
    NATIVE.write_bytes(original)
print("RESTORED: exact production native.rs; compiling and running the positive control", flush=True)
test(True)
assert not subprocess.check_output(["git", "diff", "--", str(NATIVE)], text=True)
print("PASS: post-pairing control fails without the fix and passes after exact source restoration", flush=True)
