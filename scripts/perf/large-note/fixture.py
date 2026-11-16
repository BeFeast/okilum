#!/usr/bin/env python3
"""The #653 stress vault: one 10,000-line note plus the files it links to.

    fixture.py OUTDIR          write the vault into OUTDIR
    fixture.py OUTDIR --check  also compare Large.md with the pinned SHA-256

Large.md has exactly 10,000 lines, 2,000 headings (levels 1-4), 500 links
(wikilinks, heading links, Markdown links and missing notes), four 8-column
tables of 250 rows and 200 image references (embeds and Markdown images) to
20 small PNGs. Same bytes on every host: the benchmark refuses another note.
"""
import hashlib
from pathlib import Path
import struct
import sys
import zlib

LINES = 10_000
HEADINGS = 2_000
LINKS = 500
IMAGE_REFS = 200
IMAGE_FILES = 20
TARGETS = 100
TABLES = 4
TABLE_ROWS = 250
TABLE_COLUMNS = 8
NOTE = "Large.md"
NOTE_SHA256 = "ccb97e476dce3c9e32fe0ed73b62d85516609fc68915590dd3b2afa336bb188b"


def png(width, height, seed):
    red = bytes((x * 3 + seed * 40) % 256 for x in range(width))
    rows = bytearray()
    for y in range(height):
        row = bytearray(3 * width)
        row[0::3] = red
        row[1::3] = bytes([(y * 5 + seed * 17) % 256]) * width
        row[2::3] = bytes([(seed * 61) % 256]) * width
        rows += b"\0" + row

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(bytes(rows), 9)) + chunk(b"IEND", b""))


def note():
    """Large.md as bytes, and its counts."""
    lines = ["---", "title: Large stress note", "tags: [benchmark]", "---", ""]
    links = images = 0
    table_after = {HEADINGS * (t + 1) // (TABLES + 1) for t in range(TABLES)}
    image_every = HEADINGS // IMAGE_REFS
    link_every = HEADINGS // LINKS
    for h in range(HEADINGS):
        level = 1 + (h % 4) if h else 1
        lines += [f"{'#' * level} Section {h:04} heading with *emphasis*", ""]
        text = f"Paragraph {h} with **bold**, `code`, and Unicode текст that wraps across the reading column."
        if h % link_every == 0:
            target = links % TARGETS
            if links % 10 == 9:
                text += f" See [[Missing note {links}]]."
            elif links % 10 == 8:
                text += f" See [[Target {target:03}#Part]]."
            elif links % 10 == 7:
                text += f" See [target {target}](Targets/Target%20{target:03}.md)."
            else:
                text += f" See [[Target {target:03}]]."
            links += 1
        lines += [text, ""]
        if h % image_every == image_every - 1:
            figure = images % IMAGE_FILES
            lines += [f"![[figure-{figure:02}.png]]" if images % 2
                      else f"![Figure {images}](attachments/figure-{figure:02}.png)", ""]
            images += 1
        if h in table_after:
            lines.append("| " + " | ".join(f"Column {c}" for c in range(TABLE_COLUMNS)) + " |")
            lines.append("|" + "---|" * TABLE_COLUMNS)
            for r in range(TABLE_ROWS):
                lines.append("| " + " | ".join(f"r{r}c{c} value" for c in range(TABLE_COLUMNS)) + " |")
            lines.append("")
    filler = 0
    while len(lines) < LINES:
        lines.append(f"Trailing line {filler} keeps the note at exactly {LINES} lines." if filler % 2 == 0 else "")
        filler += 1
    assert len(lines) == LINES, len(lines)
    assert links == LINKS and images == IMAGE_REFS
    headings = sum(1 for line in lines if line.startswith("#"))
    assert headings == HEADINGS, headings
    data = ("\n".join(lines) + "\n").encode()
    return data, {"lines": len(lines), "headings": headings, "links": links, "image_refs": images,
                  "tables": TABLES, "table_rows": TABLE_ROWS, "bytes": len(data)}


def write(vault, check=False):
    """Write the stress vault; return the large note's path and its counts."""
    vault = Path(vault)
    (vault / "attachments").mkdir(parents=True, exist_ok=True)
    (vault / "Targets").mkdir(exist_ok=True)
    for i in range(IMAGE_FILES):
        (vault / "attachments" / f"figure-{i:02}.png").write_bytes(png(480, 270, i))
    for i in range(TARGETS):
        (vault / "Targets" / f"Target {i:03}.md").write_text(f"# Target {i:03}\n\n## Part\n\nBody of target {i}.\n")
    data, counts = note()
    digest = hashlib.sha256(data).hexdigest()
    if check and digest != NOTE_SHA256:
        raise SystemExit(f"{NOTE} drifted: sha256 {digest}, pinned {NOTE_SHA256}")
    path = vault / NOTE
    path.write_bytes(data)
    counts["sha256"] = digest
    return path, counts


if __name__ == "__main__":
    if len(sys.argv) not in (2, 3) or sys.argv[2:] not in ([], ["--check"]):
        raise SystemExit(__doc__)
    print(write(sys.argv[1], check=sys.argv[2:] == ["--check"])[1])
