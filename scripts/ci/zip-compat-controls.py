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
MAIN_MANIFEST_SHA256 = "dd97d6dfd34c4a15f6205ea77edb92b8ccd1a29111bf48e7ec510814565aff05"
MAIN_PATCH_SHA256 = "d304d4a255a9b31e8aaae559ee5e877619bea879b7861948c41a15793ddaebdb"
MAIN_TREE_SHA256 = "5c4fe1ec68cb39db29fceef5fe1e9a05305a81550a3f697a1ffeda39693ebede"
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
