#!/usr/bin/env python3
"""Recreate/verify the checksum-pinned Windows backend plus the DirectX atlas-upload patch."""
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
            raise ValueError("symlink in generated gpui-pre-windows source")
        if path.is_file():
            result.update(path.relative_to(root).as_posix().encode() + b"\0")
            result.update(hashlib.sha256(path.read_bytes()).digest())
    return result.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", action="store_true")
    parser.add_argument("--archive", type=Path, help="use this exact archive; never access network")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    spec = repo / "scripts/gpui-pre-windows"
    manifest = json.loads((spec / "manifest.json").read_text())
    if manifest.get("changed_files") != ["src/directx_atlas.rs"]:
        raise ValueError("the approved exception is limited to src/directx_atlas.rs")
    patch = spec / manifest["patch"]
    if digest(patch.read_bytes()) != manifest["patch_sha256"]:
        raise ValueError("gpui-pre-windows patch/manifest mismatch")
    dest = repo / manifest["generated_path"]
    if dest.exists():
        if tree_digest(dest) != manifest["tree_sha256"]:
            raise ValueError("generated gpui-pre-windows differs from pinned source+patch; inspect mismatch")
    elif args.verify:
        raise ValueError("generated gpui-pre-windows missing; run scripts/gpui-pre-windows-setup.py")
    else:
        # Registry cache is a read-only archive source, never an extraction target.
        cached = sorted((Path.home() / ".cargo/registry/cache").glob("*/gpui-pre-windows-0.3.3.crate"))
        archive = args.archive or (cached[0] if cached else None)
        if archive:
            data = archive.read_bytes()
        else:
            with urllib.request.urlopen(manifest["archive_url"], timeout=60) as response:
                data = response.read()
        if digest(data) != manifest["archive_sha256"]:
            raise ValueError("gpui-pre-windows archive checksum mismatch")
        dest.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix=".gpui-pre-windows-", dir=dest.parent) as temporary:
            staged = Path(temporary)
            with tarfile.open(fileobj=io.BytesIO(data)) as source:
                for member in source.getmembers():
                    parts = Path(member.name).parts
                    if not parts or parts[0] != "gpui-pre-windows-0.3.3" or ".." in parts:
                        raise ValueError("unexpected gpui-pre-windows archive path")
                    target = staged.joinpath(*parts[1:])
                    if member.isdir():
                        target.mkdir(parents=True, exist_ok=True)
                    elif member.isfile():
                        target.parent.mkdir(parents=True, exist_ok=True)
                        target.write_bytes(source.extractfile(member).read())
                        target.chmod(member.mode & 0o777)
                    else:
                        raise ValueError("unsupported gpui-pre-windows archive member")
            before = {p.relative_to(staged).as_posix(): digest(p.read_bytes())
                      for p in staged.rglob("*") if p.is_file()}
            # Zero-context patch is safe only against the exact verified archive.
            subprocess.run(["git", "apply", "--unidiff-zero", "--unsafe-paths", "--directory=" + str(staged), str(patch)], check=True)
            after = {p.relative_to(staged).as_posix(): digest(p.read_bytes())
                     for p in staged.rglob("*") if p.is_file()}
            changed = sorted(name for name in before.keys() | after.keys()
                             if before.get(name) != after.get(name))
            if changed != manifest["changed_files"]:
                raise ValueError("Windows backend patch exceeds the DirectX atlas-upload exception")
            if tree_digest(staged) != manifest["tree_sha256"]:
                raise ValueError("gpui-pre-windows generated source/manifest mismatch")
            shutil.move(str(staged), str(dest))
    print("gpui-pre-windows ready: " + manifest["version"] + " checksum-pinned source + DirectX staging atlas upload verified")


if __name__ == "__main__":
    main()
