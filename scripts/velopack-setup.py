#!/usr/bin/env python3
"""Recreate/verify the checksum-pinned velopack source plus the hidden Windows delta-helper patch."""
import argparse
import ctypes
import errno
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
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
        # Apply as a plain patch inside the staged tree. Inside an enclosing
        # repository git silently skips paths outside the current directory
        # (TMPDIR under a checkout), so hide every repository from it.
        env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        env["GIT_CEILING_DIRECTORIES"] = str(staged.parent)
        subprocess.run(["git", "apply", "--unidiff-zero", str(Path(patch).resolve())],
                       cwd=staged, env=env, check=True)
        if tree_digest(staged) != manifest["tree_sha256"]:
            raise ValueError("velopack generated source/manifest mismatch")
        replace(staged, dest)


def exchange(first, second):
    """Atomically swap two existing paths: renameat2(RENAME_EXCHANGE) on Linux,
    renamex_np(RENAME_SWAP) on macOS. OSError when this platform or filesystem
    cannot; the caller then uses the backup-rename fallback."""
    if sys.platform.startswith("linux") or sys.platform == "darwin":
        libc = ctypes.CDLL(None, use_errno=True)
    else:
        raise OSError(errno.ENOSYS, "no atomic directory exchange on this platform")
    if sys.platform.startswith("linux") and hasattr(libc, "renameat2"):
        call = libc.renameat2
        call.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
        result = call(-100, os.fsencode(first), -100, os.fsencode(second), 2)  # AT_FDCWD, RENAME_EXCHANGE
    elif sys.platform == "darwin" and hasattr(libc, "renamex_np"):
        call = libc.renamex_np
        call.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        result = call(os.fsencode(first), os.fsencode(second), 0x2)  # RENAME_SWAP
    else:
        raise OSError(errno.ENOSYS, "no atomic directory exchange on this platform")
    if result != 0:
        code = ctypes.get_errno()
        raise OSError(code, os.strerror(code))


def backups(dest):
    """Earlier trees kept beside dest by an interrupted fallback replace()."""
    prefix = "." + dest.name + ".previous"
    if not dest.parent.is_dir():
        return []
    found = sorted(path for path in dest.parent.iterdir()
                   if path.name == prefix or path.name.startswith(prefix + "-"))
    for old in found:
        if old.is_symlink() or not old.is_dir():
            raise ValueError("velopack backup is not a plain directory; refusing to use it: " + str(old))
    return found


def recover(dest):
    """First step of every run: a fallback swap killed between its two renames
    leaves dest missing and the old tree in a backup. Put it back."""
    olds = backups(dest)
    if olds and not dest.exists():
        olds[0].rename(dest)
        print("velopack: restored " + str(dest) + " from an interrupted replacement")


def replace(staged, dest):
    """Put the verified tree at dest. With an atomic exchange there is no moment
    without dest; the old tree ends up at the staged path and goes with the
    temporary directory. Without one, dest is renamed to a backup beside it and
    the new tree renamed into place; any interruption puts dest back, and a
    killed run is repaired by recover() at the start of the next one. No earlier
    tree is deleted before the verified tree is in place."""
    olds = backups(dest)
    if dest.exists():
        try:
            exchange(staged, dest)
        except OSError:
            pass
        else:
            for old in olds:
                shutil.rmtree(old)
            return
    prefix = "." + dest.name + ".previous"
    aside = dest.with_name(prefix)
    for number in range(1, 1000):
        if not aside.exists():
            break
        aside = dest.with_name(f"{prefix}-{number}")
    else:
        raise ValueError("too many velopack backups beside " + str(dest))
    moved = False
    try:
        if dest.exists():
            dest.rename(aside)
            moved = True
        staged.rename(dest)
    except BaseException:
        if moved and not dest.exists():
            aside.rename(dest)
        raise
    # The verified tree is in place: no earlier tree is needed any more.
    for old in olds + ([aside] if moved else []):
        shutil.rmtree(old)

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
    recover(dest)
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
