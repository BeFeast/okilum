#!/usr/bin/env python3
"""Recreate/verify the checksum-pinned velopack source plus the hidden Windows delta-helper patch."""
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


def digest(data):
    return hashlib.sha256(data).hexdigest()


def tree_digest(root):
    result = hashlib.sha256()
    for path in sorted(root.rglob("*"), key=lambda path: path.relative_to(root).as_posix()):
        if path.is_symlink():
            raise ValueError("symlink in generated velopack source")
        if path.is_file():
            result.update(path.relative_to(root).as_posix().encode() + b"\0")
            result.update(hashlib.sha256(path.read_bytes()).digest())
    return result.hexdigest()


def current_digest(root):
    """Digest of an existing tree; a tree that cannot be digested is stale."""
    try:
        return tree_digest(root)
    except (OSError, ValueError):
        return None


def generate(manifest, patch, dest, archive=None):
    """Build the tree from the checksum-pinned archive and patch, verify it,
    then put it at dest, replacing a stale tree only after verification."""
    # Registry cache is a read-only archive source, never an extraction target.
    cached = sorted((Path.home() / ".cargo/registry/cache").glob("*/velopack-1.2.161.crate"))
    archive = archive or (cached[0] if cached else None)
    if archive:
        data = archive.read_bytes()
    else:
        with urllib.request.urlopen(manifest["archive_url"], timeout=60) as response:
            data = response.read()
    if digest(data) != manifest["archive_sha256"]:
        raise ValueError("velopack archive checksum mismatch")
    dest.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".velopack-", dir=dest.parent) as temporary:
        staged = Path(temporary) / "source"
        staged.mkdir()
        with tarfile.open(fileobj=io.BytesIO(data)) as source:
            for member in source.getmembers():
                parts = Path(member.name).parts
                if not parts or parts[0] != "velopack-1.2.161" or ".." in parts:
                    raise ValueError("unexpected velopack archive path")
                target = staged.joinpath(*parts[1:])
                if member.isdir():
                    target.mkdir(parents=True, exist_ok=True)
                elif member.isfile():
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(source.extractfile(member).read())
                    target.chmod(member.mode & 0o777)
                else:
                    raise ValueError("unsupported velopack archive member")
        # Zero-context patch is safe only against the exact verified archive.
        subprocess.run(["git", "apply", "--unidiff-zero", "--unsafe-paths", "--directory=" + str(staged), str(patch)], check=True)
        if tree_digest(staged) != manifest["tree_sha256"]:
            raise ValueError("velopack generated source/manifest mismatch")
        replace(staged, dest)


def replace(staged, dest):
    """Put the verified tree at dest. The old tree is kept beside dest (not in
    the temporary directory, whose cleanup would delete it) until the new one
    is in place, and restored on any interruption, KeyboardInterrupt included."""
    backup = dest.with_name("." + dest.name + ".previous")
    if backup.is_symlink():
        raise ValueError("velopack backup path is a symlink; refusing to use it")
    if backup.exists():
        if dest.exists():
            shutil.rmtree(backup)  # leftover of an interrupted earlier cleanup
        else:
            backup.rename(dest)  # an interrupted swap: put the old tree back first
    moved = False
    try:
        if dest.exists():
            dest.rename(backup)
            moved = True
        staged.rename(dest)
    except BaseException:
        if moved and not dest.exists():
            backup.rename(dest)
        raise
    if moved:
        shutil.rmtree(backup)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", action="store_true")
    parser.add_argument("--archive", type=Path, help="use this exact archive; never access network")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    spec = repo / "scripts/velopack"
    manifest = json.loads((spec / "manifest.json").read_text())
    patch = spec / manifest["patch"]
    if digest(patch.read_bytes()) != manifest["patch_sha256"]:
        raise ValueError("velopack patch/manifest mismatch")
    dest = repo / manifest["generated_path"]
    if not dest.exists():
        if args.verify:
            raise ValueError("generated velopack missing; run scripts/velopack-setup.py")
        generate(manifest, patch, dest, args.archive)
    elif current_digest(dest) != manifest["tree_sha256"]:
        if args.verify:
            raise ValueError("generated velopack differs from pinned source+patch; run scripts/velopack-setup.py")
        # vendor/ is not committed, so a kept worktree (the Mac Sprint one)
        # still holds the tree of an older patch after main moves on (14.11).
        # The tree is generated: rebuild it from the pinned archive and patch.
        if dest.is_symlink() or not dest.is_dir():
            raise ValueError("generated velopack path is not a plain directory; refusing to replace it")
        print("velopack: generated tree is stale for the current patch; regenerating " + str(dest))
        generate(manifest, patch, dest, args.archive)
    print("velopack ready: " + manifest["version"] + " checksum-pinned source + hidden delta helper verified")


if __name__ == "__main__":
    main()
