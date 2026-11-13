#!/usr/bin/env python3
"""Recreate/verify the checksum-pinned GPUI macOS crates plus the approved first-window patches.

Oleg approved two core GPUI patches on 2026-11-13 (#2825): no tab-group query for windows
without tabbing, and the Metal shader library/pipelines built off the first window's
critical path. Each crate is extracted from its exact crates.io archive, the listed
patches are applied in order, and the result must match the pinned tree digest.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request

SPECS = ("gpui-pre-macos",)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def tree_digest(root):
    result = hashlib.sha256()
    for path in sorted(root.rglob("*"), key=lambda path: path.relative_to(root).as_posix()):
        if path.is_symlink():
            raise ValueError("symlink in generated GPUI macOS source")
        if path.is_file():
            result.update(path.relative_to(root).as_posix().encode() + b"\0")
            result.update(hashlib.sha256(path.read_bytes()).digest())
    return result.hexdigest()


def files(root):
    return {p.relative_to(root).as_posix(): digest(p.read_bytes()) for p in root.rglob("*") if p.is_file()}


def prepare(repo, name, archive_override, verify):
    spec = repo / "scripts" / name
    manifest = json.loads((spec / "manifest.json").read_text())
    package = manifest["package"]
    if package != name:
        raise ValueError(f"{name}: manifest names {package}")
    patches = []
    for entry in manifest["patches"]:
        patch = spec / entry["path"]
        if digest(patch.read_bytes()) != entry["sha256"]:
            raise ValueError(f"{package} patch/manifest mismatch: {entry['path']}")
        patches.append(patch)
    dest = repo / manifest["generated_path"]
    if dest.exists():
        if tree_digest(dest) != manifest["tree_sha256"]:
            raise ValueError(f"generated {package} differs from pinned source+patches; inspect mismatch")
        return manifest
    if verify:
        raise ValueError(f"generated {package} missing; run scripts/gpui-pre-apple-setup.py")
    crate = f"{package}-{manifest['version']}"
    # Registry cache is a read-only archive source, never an extraction target.
    cached = sorted((Path.home() / ".cargo/registry/cache").glob(f"*/{crate}.crate"))
    archive = archive_override or (cached[0] if cached else None)
    if archive:
        data = archive.read_bytes()
    else:
        with urllib.request.urlopen(manifest["archive_url"], timeout=60) as response:
            data = response.read()
    if digest(data) != manifest["archive_sha256"]:
        raise ValueError(f"{package} archive checksum mismatch")
    dest.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=f".{package}-", dir=dest.parent) as temporary:
        staged = Path(temporary)
        with tarfile.open(fileobj=io.BytesIO(data)) as source:
            for member in source.getmembers():
                parts = Path(member.name).parts
                if not parts or parts[0] != crate or ".." in parts:
                    raise ValueError(f"unexpected {package} archive path")
                target = staged.joinpath(*parts[1:])
                if member.isdir():
                    target.mkdir(parents=True, exist_ok=True)
                elif member.isfile():
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(source.extractfile(member).read())
                    target.chmod(member.mode & 0o777)
                else:
                    raise ValueError(f"unsupported {package} archive member")
        before = files(staged)
        for patch in patches:
            # Context patches apply only to the exact verified archive.
            subprocess.run(["git", "apply", "--unsafe-paths", "--directory=" + str(staged), str(patch)], check=True)
        after = files(staged)
        changed = sorted(name for name in before.keys() | after.keys() if before.get(name) != after.get(name))
        if changed != manifest["changed_files"]:
            raise ValueError(f"{package} patches change {changed}, not the approved {manifest['changed_files']}")
        if tree_digest(staged) != manifest["tree_sha256"]:
            raise ValueError(f"{package} generated source/manifest mismatch")
        shutil.move(str(staged), str(dest))
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", action="store_true")
    parser.add_argument("--archive-dir", type=Path, help="use exact .crate archives from this directory; never access network")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    for name in SPECS:
        archive = None
        if args.archive_dir:
            version = json.loads((repo / "scripts" / name / "manifest.json").read_text())["version"]
            archive = args.archive_dir / f"{name}-{version}.crate"
        manifest = prepare(repo, name, archive, args.verify)
        print(f"{name} ready: {manifest['version']} checksum-pinned source + {len(manifest['patches'])} approved first-window patch(es) verified")


if __name__ == "__main__":
    main()
