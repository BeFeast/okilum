#!/usr/bin/env python3
"""Install the canonical Board guide into the existing okilum.app source tree.

Usage: python3 docs/help/board/install-site.py /path/to/okilum-site
Then run that site's existing build.py and deployment command.
"""
from pathlib import Path
import shutil
import sys

ROOT = Path(__file__).resolve().parent
FILES = ("assets/Manrope.ttf", "assets/OFL.txt", "index.html", "captions.vtt", "assets/board-tutorial.mp4", "assets/board-saved.png")
HOOK = "\n# Canonical product Help, installed from the Okilum repository.\nif (SRC / 'help').is_dir():\n    shutil.copytree(SRC / 'help', OUT / 'help', dirs_exist_ok=True)\n"


def install(site):
    build = site / "build.py"
    shell = site / "src/shell.html"
    source = build.read_text()
    if "SRC = ROOT / 'src'" not in source or "OUT = ROOT / 'public'" not in source:
        raise ValueError("Unsupported site build layout; no files changed")
    marker = '<nav class="footer-links" aria-label="Footer navigation">'
    for path in (shell, site / "src/home.html"):
        text = path.read_text()
        if marker not in text:
            raise ValueError(f"Missing footer navigation in {path}; no files changed")
    for name in FILES:
        destination = site / "src/help/board" / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / name, destination)
    if HOOK not in source:
        build.write_text(source + HOOK)
    link = '<a href="/help/board/">Board help</a>'
    # Both templates have a footer navigation; install once, preserving the
    # existing home/download/legal links and every download/release setting.
    for path in (shell, site / "src/home.html"):
        text = path.read_text()
        if link not in text:
            path.write_text(text.replace(marker, marker + link, 1))


if __name__ == "__main__":
    install(Path(sys.argv[1]).resolve())
