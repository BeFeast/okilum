#!/usr/bin/env python3
"""Fail when a shipped asset still carries the Tessera mark (#977).

Two checks over the tracked tree:
1. The app and Inbox icons are byte-identical to the approved Okilum
   "O / Kontur" masters (hashes below). Every platform icon is derived from them:
   in-app symbol, Windows .ico, macOS .icns, Linux hicolor icon, Inbox web icon.
2. No shipped text asset contains a signature of the old Tessera "Open join" mark.
   Design history and documentation screenshots are excluded on purpose.

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
    if problems:
        print("Tessera brand assets remain:\n  " + "\n  ".join(problems))
        return 1
    print(f"Okilum brand check: {len(OKILUM)} icons match the approved masters; no Tessera mark in shipped assets")
    return 0


if __name__ == "__main__":
    sys.exit(main())
