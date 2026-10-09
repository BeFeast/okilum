#!/usr/bin/env python3
"""Fail when the tree still carries the Tessera name or mark (#977, #980, #987).

Four checks over the tracked tree:
1. The app and Inbox icons are byte-identical to the approved Okilum
   "O / Kontur" masters (hashes below). Every platform icon is derived from them:
   in-app symbol, Windows .ico, macOS .icns, Linux hicolor icon, Inbox web icon.
2. No shipped text asset contains a signature of the old Tessera "Open join" mark.
   Design history and documentation screenshots are excluded on purpose.
3. No tracked image or binary asset outside history and fixtures is a known
   Tessera image (tessera-images.sha256) or carries the name in its bytes
   (PNG text chunks, ICO/ICNS/PDF metadata). Pixels are not read, so a newly
   drawn Tessera image must be added to the hash list by whoever finds it.
4. No tracked file outside vendor/ and this detector directory contains the
   name "tessera" in any case, as text or bytes, or in its path (#987).

    python3 scripts/rebrand/check_brand.py
"""
import hashlib
import subprocess
import sys

OKILUM = {
    "crates/okilum-shell/assets/brand/symbol-primary.svg": "445f7d4907f5df3763fc2d9ac774ddf764852d7004b4244aa7e24dd37cfaccfe",
    "crates/okilum-shell/assets/brand/symbol-reversed.svg": "6a0fedcb83e59c9384f31a1c4ae0d665c26c2ecb9091a4e2dbb9fb6a4d42beb4",
    "crates/okilum-shell/assets/brand/app-icon-light.svg": "15ae26cf29c68814bd9d05f1ee0df7b865d30951c778977ca40c22b7a5f5f54c",
    "crates/okilum-shell/assets/brand/app-icon-dark.svg": "fba80d5bf2a661ba9fb7e7839c854d1bdcaec93031fbdb34057281e9196a9888",
    "web/inbox/icon.svg": "15ae26cf29c68814bd9d05f1ee0df7b865d30951c778977ca40c22b7a5f5f54c",
    "web/inbox/icon-dark.svg": "fba80d5bf2a661ba9fb7e7839c854d1bdcaec93031fbdb34057281e9196a9888",
}
# Path data and labels of the Tessera "Open join" mark.
OLD_MARK = ("M9.5 10H54.5", "M25 45L39 31", "Open join")
# Not shipped: design history, prototypes, documentation, research, this script.
NOT_SHIPPED = ("design/", "docs/", "experiments/", "fixtures/", "scripts/rebrand/")
# History and test input keep the old name on purpose.
BINARY_HISTORY = ("docs/archive/", "docs/research/", "docs/upstream/", "experiments/", "fixtures/", "vendor/", "scripts/rebrand/")
BINARY_SUFFIXES = (".png", ".ico", ".icns", ".jpg", ".jpeg", ".webp", ".gif", ".bmp", ".pdf")
OLD_IMAGES = "scripts/rebrand/tessera-images.sha256"
# Only the detector may name the old brand; vendor/ is third-party code.
NAME_ALLOWED = ("scripts/rebrand/", "vendor/")
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
            problems.append(f"{path}: a known Tessera image ({OLD_IMAGES})")
        elif b"tessera" in data.lower():
            problems.append(f"{path}: contains the Tessera name in its bytes")
    for path in files:
        if not path or path.startswith(NOT_SHIPPED) or not path.endswith(TEXT_SUFFIXES):
            continue
        try:
            text = open(path, encoding="utf-8", errors="ignore").read()
        except (FileNotFoundError, IsADirectoryError):
            continue
        for mark in OLD_MARK:
            if mark in text:
                problems.append(f"{path}: contains the Tessera mark ({mark!r})")
    named = 0
    for path in files:
        if not path or path.startswith(NAME_ALLOWED):
            continue
        if "tessera" in path.lower():
            problems.append(f"{path}: the path names Tessera")
        try:
            data = open(path, "rb").read()
        except (FileNotFoundError, IsADirectoryError):
            continue
        named += 1
        if b"tessera" in data.lower():
            line = next((n for n, text in enumerate(data.split(b"\n"), 1) if b"tessera" in text.lower()), 0)
            problems.append(f"{path}:{line}: names Tessera")
    if problems:
        print("Tessera brand assets remain:\n  " + "\n  ".join(problems))
        return 1
    print(
        f"Okilum brand check: {len(OKILUM)} icons match the approved masters; no Tessera mark in shipped assets; "
        f"{binaries} images and binary assets clean; no Tessera name in {named} tracked files"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
