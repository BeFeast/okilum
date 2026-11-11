#!/usr/bin/env python3
"""Runtime negative control for #2821; run only on a hosted Rust builder."""
from pathlib import Path
import subprocess


def main():
    path = Path("crates/okilum-core/src/vault_conventions.rs")
    source = path.read_text()
    anchor = 'path.starts_with("@Assets/")'
    if source.count(anchor) != 1:
        raise RuntimeError("@Assets classification negative-control anchor changed")
    command = ["cargo", "test", "--offline", "--locked", "-p", "okilum-core", "--test=at_vault",
               "windows_issue2821_assets_are_not_notes_but_resolve_and_render"]
    try:
        # Reproduce the old classifier while retaining all new assertions/fixtures.
        path.write_text(source.replace(anchor, 'path.starts_with("unmatched-legacy-control/")'))
        result = subprocess.run([*command, "--", "--exact", "--nocapture"], text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=480)
        print(result.stdout, flush=True)
        if (result.returncode == 0 or "1 failed" not in result.stdout
                or "@Assets Markdown must not enter note lists or counts" not in result.stdout):
            raise RuntimeError("Old @Assets behavior did not fail the actual note-list assertion")
    finally:
        path.write_text(source)
    subprocess.run([*command, "--", "--exact", "--nocapture"], check=True, timeout=480)
    print("PASS #2821 old-behavior runtime failure and restored positive control", flush=True)


if __name__ == "__main__":
    main()
