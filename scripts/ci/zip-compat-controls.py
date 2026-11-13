#!/usr/bin/env python3
"""Prove split ZIP ownership, unchanged main Velopack, and Reader regressions."""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
# Vendored Velopack as reviewed in PR 2855 (helper publication/recovery). Any further
# updater change must update these pins in its own reviewed PR.
MAIN_MANIFEST_SHA256 = "b951e290abf96d39f7afa4eb9dbf7ee1415790262bcedc532137548ec64c7267"
MAIN_PATCH_SHA256 = "3984ed228c635dc45bc31502042deae65ed76d9f1370aef89bab913c036c68c4"
MAIN_TREE_SHA256 = "6278e29e4aa5cad6ee1b48fee6783d02f360b814e6ba3604e954968a5c0efe31"
COMMAND = ["cargo", "test", "--locked", "-p", "okilum-core", "--lib"]


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    for name, expected in [("manifest.json", MAIN_MANIFEST_SHA256), ("hidden-delta.diff", MAIN_PATCH_SHA256)]:
        assert hashlib.sha256((ROOT / "scripts/velopack" / name).read_bytes()).hexdigest() == expected, name
    spec = importlib.util.spec_from_file_location("velopack_setup", ROOT / "scripts/velopack-setup.py")
    setup = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(setup)
    assert setup.tree_digest(ROOT / "vendor/velopack-1.2.161") == MAIN_TREE_SHA256
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1"], cwd=ROOT))
    packages = {p["id"]: p for p in metadata["packages"]}
    zip_ids = {p["version"]: p["id"] for p in packages.values() if p["name"] == "zip"}
    assert set(zip_ids) == {"8.6.0", "9.5.1"}, zip_ids
    owners = {packages[n["id"]]["name"] for n in metadata["resolve"]["nodes"]
              if any(d["pkg"] == zip_ids["8.6.0"] for d in n["deps"])}
    assert owners == {"velopack"}, owners
    print("ZIP_SPLIT_GRAPH_PASS zip8=Velopack-only zip9=Reader vendor=pinned-byte-identical", flush=True)
    source = ROOT / "crates/okilum-core/src/archive.rs"
    original = source.read_bytes()
    cases = [
        (b"let utf8 = file.flags().as_u16() & (1 << 11) != 0;",
         b"let utf8 = std::str::from_utf8(file.name_raw()).is_ok();",
         "archive::tests::utf8_flag_controls_ambiguous_names_and_extraction_paths"),
        (b"if raw_symlink || file.is_symlink()", b"if file.is_symlink()",
         "archive::tests::dos_origin_unix_symlink_attributes_are_still_refused"),
    ]
    try:
        for before, after, test in cases:
            assert original.count(before) == 1
            source.write_bytes(original.replace(before, after))
            result = subprocess.run(COMMAND + [test, "--", "--exact", "--nocapture"], cwd=ROOT,
                                    text=True, encoding="utf-8", stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            print(result.stdout, flush=True)
            assert result.returncode == 101 and f"{test} ... FAILED" in result.stdout, test
            print("READER_REVERT_CONTROL_FIRED " + test, flush=True)
    finally:
        source.write_bytes(original)
    subprocess.run(COMMAND + ["archive::tests::", "--", "--nocapture"], cwd=ROOT, check=True)


if __name__ == "__main__":
    main()
