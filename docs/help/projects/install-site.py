#!/usr/bin/env python3
"""Install the canonical Projects guide into the existing okilum.app source tree.

Usage: python3 docs/help/projects/install-site.py /path/to/okilum-site
Then run that site's existing build.py and deployment command.
"""
from pathlib import Path
import shutil
import sys

ROOT = Path(__file__).resolve().parent
FILES = ("assets/Manrope.ttf", "assets/OFL.txt", "index.html", "captions.vtt", "assets/projects-tutorial.mp4", "assets/projects-board.png")
# Same hook text as docs/help/board/install-site.py: one copy step serves every guide.
HOOK = "\n# Canonical product Help, installed from the Okilum repository.\nif (SRC / 'help').is_dir():\n    shutil.copytree(SRC / 'help', OUT / 'help', dirs_exist_ok=True)\n"
MARKER = '<nav class="footer-links" aria-label="Footer navigation">'
LINK = '<a href="/help/projects/">Projects help</a>'


def install(site):
    build = site / "build.py"
    templates = (site / "src/shell.html", site / "src/home.html")
    source = build.read_text()
    if "SRC = ROOT / 'src'" not in source or "OUT = ROOT / 'public'" not in source:
        raise ValueError("Unsupported site build layout; no files changed")
    for path in templates:
        if MARKER not in path.read_text():
            raise ValueError(f"Missing footer navigation in {path}; no files changed")
    for name in FILES:
        destination = site / "src/help/projects" / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / name, destination)
    if HOOK not in source:
        build.write_text(source + HOOK)
    # Both templates have a footer navigation; install once, preserving every
    # existing link (including Board help) and all download/release settings.
    for path in templates:
        text = path.read_text()
        if LINK not in text:
            path.write_text(text.replace(MARKER, MARKER + LINK, 1))


if __name__ == "__main__":
    install(Path(sys.argv[1]).resolve())
