#!/usr/bin/env python3
"""The 10 MiB note of the #2094 speed budget, pinned like the editor fixtures.

It reuses the #1639 generator (original rotating prose, the same six Markdown
constructs, 96-column wrapping): exactly 10,485,760 ASCII bytes in 10,240
sections, about 205,000 lines. ``tiny.md`` stays the editor's pinned 3 KiB note.
``manifest.json`` next to this file pins both; the collector refuses drift.
"""

import hashlib
import json
from pathlib import Path
import re
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "editor"))

import fixtures  # noqa: E402

LARGE = "ten-mib.md"
SMALL = "tiny.md"


def notes() -> dict[str, bytes]:
    return {
        SMALL: fixtures.notes()[SMALL],
        LARGE: fixtures._document("Ten mebibytes of field notes", 10 * 1024 * 1024, 10_240),
    }


def manifest(files: dict[str, bytes]) -> dict[str, dict[str, int | str]]:
    return {name: {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data),
                   "lines": data.count(b"\n"),
                   "sections": len(re.findall(rb"^#{1,6} ", data, flags=re.MULTILINE))}
            for name, data in files.items()}


def checked() -> dict[str, bytes]:
    """The notes, or ValueError when they differ from the pinned manifest."""
    files = notes()
    pinned = json.loads((HERE / "manifest.json").read_text(encoding="utf-8"))
    if manifest(files) != pinned:
        raise ValueError("speed budget notes differ from scripts/perf/speed/manifest.json")
    return files


if __name__ == "__main__":
    print(json.dumps(manifest(notes()), indent=2, sort_keys=True))
