#!/usr/bin/env python3
"""Verify the pinned GPUI core with value-copyable shaped glyphs (#2803)."""
import argparse
import importlib.util
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", action="store_true")
    parser.add_argument("--archive", type=Path, help="use this exact archive without network access")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    # Reuse the existing archive/patch/tree verification and atomic extraction.
    spec = importlib.util.spec_from_file_location(
        "pinned_gpui", repo / "scripts/gpui-pre-apple-setup.py"
    )
    setup = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(setup)
    manifest = setup.prepare(repo, "gpui-pre", args.archive, args.verify)
    print(f"gpui-pre ready: {manifest['version']} checksum-pinned source + Copy shaped glyphs verified")


if __name__ == "__main__":
    main()
