#!/usr/bin/env python3
"""Fail when the tree still carries the old product name or mark (#977, #980, #987).

Four checks over the tracked tree:
1. The app and Inbox icons are byte-identical to the approved Okilum
   "O / Kontur" masters (hashes below). Every platform icon is derived from them:
   in-app symbol, Windows .ico, macOS .icns, Linux hicolor icon, Inbox web icon.
2. No shipped text asset contains a signature of the old "Open join" mark.
   Design history and documentation screenshots are excluded on purpose.
3. No tracked image or binary asset outside history and fixtures is a known
   old-brand image (old-brand-images.sha256) or carries the name in its bytes
   (PNG text chunks, ICO/ICNS/PDF metadata). Pixels are not read, so a newly
   drawn old-brand image must be added to the hash list by whoever finds it.
4. No tracked file outside vendor/ contains the old name in any case, in its
   path, as text or bytes, inside a zip container (docx, xlsx, zip) or inside a
   base64 run (#987). This script spells the name in two parts, so it passes
   its own check and a plain `git grep` of the tree finds nothing.

    python3 scripts/rebrand/check_brand.py
"""
import base64
import binascii
import hashlib
import io
import re
import subprocess
import sys
import zipfile

# The old product name, in two parts so this file never contains it.
OLD_NAME = "tes" + "sera"
OLD_TITLE = OLD_NAME.capitalize()

OKILUM = {
    "crates/okilum-shell/assets/brand/symbol-primary.svg": "445f7d4907f5df3763fc2d9ac774ddf764852d7004b4244aa7e24dd37cfaccfe",
    "crates/okilum-shell/assets/brand/symbol-reversed.svg": "6a0fedcb83e59c9384f31a1c4ae0d665c26c2ecb9091a4e2dbb9fb6a4d42beb4",
    "crates/okilum-shell/assets/brand/app-icon-light.svg": "15ae26cf29c68814bd9d05f1ee0df7b865d30951c778977ca40c22b7a5f5f54c",
    "crates/okilum-shell/assets/brand/app-icon-dark.svg": "fba80d5bf2a661ba9fb7e7839c854d1bdcaec93031fbdb34057281e9196a9888",
    "web/inbox/icon.svg": "15ae26cf29c68814bd9d05f1ee0df7b865d30951c778977ca40c22b7a5f5f54c",
    "web/inbox/icon-dark.svg": "fba80d5bf2a661ba9fb7e7839c854d1bdcaec93031fbdb34057281e9196a9888",
}
# Path data and labels of the old "Open join" mark.
OLD_MARK = ("M9.5 10H54.5", "M25 45L39 31", "Open join")
# Not shipped: design history, prototypes, documentation, research, this script.
NOT_SHIPPED = ("design/", "docs/", "experiments/", "fixtures/", "scripts/rebrand/")
# History and test input keep the old name on purpose.
BINARY_HISTORY = ("docs/archive/", "docs/research/", "docs/upstream/", "experiments/", "fixtures/", "vendor/", "scripts/rebrand/")
BINARY_SUFFIXES = (".png", ".ico", ".icns", ".jpg", ".jpeg", ".webp", ".gif", ".bmp", ".pdf")
OLD_IMAGES = "scripts/rebrand/old-brand-images.sha256"
# vendor/ is third-party code.
NAME_ALLOWED = ("vendor/",)
# Bytes that cannot change without a secret: the Arch signing key's user id names
# the old brand inside its armored base64, and rewriting it needs the private key.
ENCODED_ALLOWED = ("scripts/arch/okilum-signing-key.asc",)
# A base64 run, joined across line wraps (armored keys, MIME) before decoding.
BASE64_RUN = re.compile(rb"[A-Za-z0-9+/]{16,}={0,2}(?:\r?\n[A-Za-z0-9+/]{4,}={0,2})*")


def encoded_name(data):
    """Where the old name hides inside a zip member or a base64 run, if anywhere."""
    if data[:4] == b"PK\x03\x04":
        try:
            with zipfile.ZipFile(io.BytesIO(data)) as archive:
                for member in archive.namelist():
                    if OLD_NAME.encode() in archive.read(member).lower():
                        return f"zip member {member}"
        except (zipfile.BadZipFile, NotImplementedError):
            pass
    for run in BASE64_RUN.finditer(data):
        token = run.group(0).replace(b"\r", b"").replace(b"\n", b"")
        try:
            decoded = base64.b64decode(token + b"=" * (-len(token) % 4))
        except (binascii.Error, ValueError):
            continue
        if OLD_NAME.encode() in decoded.lower():
            line = data.count(b"\n", 0, run.start()) + 1
            return f"base64 at line {line}"
    return None
TEXT_SUFFIXES = (".svg", ".html", ".css", ".js", ".json", ".webmanifest", ".rs", ".desktop", ".plist", ".xml", ".rc")


def main():
    problems = []
    for path, expected in OKILUM.items():
        try:
            actual = hashlib.sha256(open(path, "rb").read()).hexdigest()
        except FileNotFoundError:
            problems.append(f"{path}: missing")
            continue
        if actual != expected:
            problems.append(f"{path}: not the approved Okilum asset ({actual[:12]})")
    files = subprocess.run(["git", "ls-files", "-z"], check=True, capture_output=True, text=True).stdout.split("\0")
    old_images = {line.split()[0] for line in open(OLD_IMAGES) if line.strip() and not line.startswith("#")}
    binaries = 0
    for path in files:
        if not path.lower().endswith(BINARY_SUFFIXES) or path.startswith(BINARY_HISTORY):
            continue
        if "/tests/fixtures/" in f"/{path}" or "/testdata/" in f"/{path}":
            continue
        try:
            data = open(path, "rb").read()
        except FileNotFoundError:
            continue
        binaries += 1
        if hashlib.sha256(data).hexdigest() in old_images:
            problems.append(f"{path}: a known {OLD_TITLE} image ({OLD_IMAGES})")
        elif OLD_NAME.encode() in data.lower():
            problems.append(f"{path}: contains the {OLD_TITLE} name in its bytes")
    for path in files:
        if not path or path.startswith(NOT_SHIPPED) or not path.endswith(TEXT_SUFFIXES):
            continue
        try:
            text = open(path, encoding="utf-8", errors="ignore").read()
        except (FileNotFoundError, IsADirectoryError):
            continue
        for mark in OLD_MARK:
            if mark in text:
                problems.append(f"{path}: contains the {OLD_TITLE} mark ({mark!r})")
    named = 0
    for path in files:
        if not path or path.startswith(NAME_ALLOWED):
            continue
        if OLD_NAME in path.lower():
            problems.append(f"{path}: the path names {OLD_TITLE}")
        try:
            data = open(path, "rb").read()
        except (FileNotFoundError, IsADirectoryError):
            continue
        named += 1
        if OLD_NAME.encode() in data.lower():
            line = next((n for n, text in enumerate(data.split(b"\n"), 1) if OLD_NAME.encode() in text.lower()), 0)
            problems.append(f"{path}:{line}: names {OLD_TITLE}")
        elif path not in ENCODED_ALLOWED:
            hidden = encoded_name(data)
            if hidden:
                problems.append(f"{path}: names {OLD_TITLE} ({hidden})")
    if problems:
        print(f"{OLD_TITLE} brand assets remain:\n  " + "\n  ".join(problems))
        return 1
    print(
        f"Okilum brand check: {len(OKILUM)} icons match the approved masters; no {OLD_TITLE} mark in shipped assets; "
        f"{binaries} images and binary assets clean; no {OLD_TITLE} name in {named} tracked files"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
