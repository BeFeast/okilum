#!/usr/bin/env python3
"""Run the exact predecessor recovery reader against a newly produced v2 record."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

REPO = Path(__file__).resolve().parents[2]
PREDECESSOR = "7ed3d9d"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="New isolated evidence directory")
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    project = output / "harness"
    source = project / "src" / "brain"
    (source / "editor_recovery").mkdir(parents=True)
    old = subprocess.check_output(
        ["git", "show", f"{PREDECESSOR}:crates/tessera-shell/src/brain/editor_recovery.rs"],
        cwd=REPO,
    )
    (source / "old_recovery.rs").write_bytes(old)
    for name in ["editor_recovery.rs", "editor_recovery/auto_resolution.rs", "merge_preview.rs"]:
        shutil.copyfile(REPO / "crates/tessera-shell/src/brain" / name, source / name)
    shutil.copyfile(Path(__file__).with_name("compatibility-main.rs"), project / "src/main.rs")
    (project / "Cargo.toml").write_text('''[package]
name = "source206-compat"
version = "0.1.0"
edition = "2021"
[workspace]
[dependencies]
base64 = "0.22"
serde_json = "1"
sha2 = "0.10"
uuid = { version = "1", features = ["v4"] }
similar = { version = "=2.7.0", default-features = false }
''')
    manifest = {
        "predecessor_commit": subprocess.check_output(
            ["git", "rev-parse", PREDECESSOR], cwd=REPO, text=True
        ).strip(),
        "files": {
            str(p.relative_to(project)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted((project / "src").rglob("*.rs"))
        },
    }
    (output / "source-hashes.json").write_text(json.dumps(manifest, indent=2) + "\n")
    # Separate Cargo target: this small helper must not invalidate a native build graph.
    with (output / "build.log").open("w") as log:
        completed = subprocess.run(
            ["cargo", "run", "--manifest-path", str(project / "Cargo.toml"),
             "--target-dir", str(project / "target"), "--", str(output / "fixture")],
            cwd=REPO, stdout=subprocess.PIPE, stderr=log, text=True, check=True,
        )
    result = json.loads(completed.stdout)
    assert result["verdict"] == "PASS"
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
