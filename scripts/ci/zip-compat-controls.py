#!/usr/bin/env python3
"""Prove split ZIP ownership, unchanged main Velopack, and Reader regressions."""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
# Main 718d23d8b799bd50569ba0b8ff40e784afb4ed0c. Changing the updater is a separate PR.
MAIN_MANIFEST_SHA256 = "df75037cdc98ab2d568a3cf4940c82fd0ffb9fb9bb7107193da8b2f3be31fc0f"
MAIN_PATCH_SHA256 = "b43f52b39928715e53a8bf7dc8afb13dfbd9a1fedadb0d07ff49a254994b03d7"
MAIN_TREE_SHA256 = "9dbbe586d66712c914fc7017d1f0f82fba63f81b9227ac0eb198e388ec646ba8"
COMMAND = ["cargo", "test", "--locked", "-p", "okilum-core", "--lib"]


def main():
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
    print("ZIP_SPLIT_GRAPH_PASS zip8=Velopack-only zip9=Reader vendor=main-byte-identical", flush=True)
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
