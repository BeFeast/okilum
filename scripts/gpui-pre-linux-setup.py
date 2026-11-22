#!/usr/bin/env python3
"""Recreate/verify checksum-pinned Linux external data drops (#1615)."""
import argparse
import importlib.util
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", action="store_true")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    spec = importlib.util.spec_from_file_location("pinned_gpui", repo / "scripts/gpui-pre-apple-setup.py")
    setup = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(setup)
    manifest = setup.prepare(repo, "gpui-pre-linux", None, args.verify)
    print(f"gpui-pre-linux ready: {manifest['version']} checksum-pinned external data drops verified")


if __name__ == "__main__":
    main()
