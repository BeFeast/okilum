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
# The control: a short note in the same vault, opened the same way, so the
# launch cost every note pays is not counted against the large one.
SMALL = "Small.md"
SMALL_TEXT = "".join(f"## Small section {i}\n\nA short paragraph {i}.\n\n" for i in range(5))
NOTE_SHA256 = "ccb97e476dce3c9e32fe0ed73b62d85516609fc68915590dd3b2afa336bb188b"
# The code-block note (#3156): 10,000 lines of fenced Rust, Python,
# JavaScript and JSON under headings, for syntax-palette A/B runs.
CODE_NOTE = "Code.md"
CODE_NOTE_SHA256 = "41c31aa735dfe7b493ad7ab9517ff2308ae8d2d05248eb829fa85882a6535dce"
CODE_SAMPLES = {
    "rust": [
        "/// Sums the weights of section {i}.",
        "pub fn weigh_{i}(items: &[Item], limit: usize) -> Option<u64> {{",
        "    let mut total: u64 = 0; // running sum",
        "    for (index, item) in items.iter().enumerate().take(limit) {{",
        "        if item.name.starts_with(\"skip-{i}\") {{",
        "            continue;",
        "        }}",
        "        total += item.weight * {i} + index as u64;",
        "    }}",
        "    match total {{",
        "        0 => None,",
        "        n if n > 0x{i:04x} => Some(n - 1),",
        "        n => Some(n),",
        "    }}",
        "}}",
        "const LIMIT_{i}: usize = {i} * 16;",
    ],
    "python": [
        "# Section {i}: parse and count records.",
        "import json",
        "from dataclasses import dataclass",
        "",
        "@dataclass",
        "class Record{i}:",
        "    name: str",
        "    weight: float = {i}.5",
        "",
        "def count_{i}(rows: list[dict]) -> int:",
        "    \"\"\"Count rows whose name matches.\"\"\"",
        "    total = 0",
        "    for row in rows:",
        "        if row.get(\"name\", \"\").startswith(\"r{i}\") and row[\"weight\"] > 3.14:",
        "            total += 1",
        "    return total",
    ],
    "javascript": [
        "// Section {i}: render a list of entries.",
        "export async function render{i}(entries, {{ limit = {i} }} = {{}}) {{",
        "  const out = [];",
        "  for (const [index, entry] of entries.entries()) {{",
        "    if (index >= limit || entry === null) break;",
        "    out.push(`${{entry.title}} #{i}: ${{entry.count * 2}}`);",
        "  }}",
        "  const total = out.length > 0 ? out.length : -1;",
        "  return {{ total, items: out, ok: true, ratio: 0.{i} }};",
        "}}",
        "",
        "class View{i} extends Base {{",
        "  constructor(root) {{ super(root); this.root = root; }}",
        "  get size() {{ return this.root?.children.length ?? 0; }}",
        "}}",
        "const pattern{i} = /^item-{i}-(\\d+)$/u;",
    ],
    "json": [
        "{{",
        "  \"section\": {i},",
        "  \"name\": \"code-{i}\",",
        "  \"enabled\": true,",
        "  \"ratio\": 0.{i},",
        "  \"owner\": null,",
        "  \"tags\": [\"alpha\", \"beta\", \"gamma-{i}\"],",
        "  \"limits\": {{",
        "    \"min\": -{i},",
        "    \"max\": {i}00,",
        "    \"step\": 1.5e3",
        "  }},",
        "  \"nested\": [{{ \"id\": 1 }}, {{ \"id\": 2 }}],",
        "  \"escaped\": \"tab\\tnew\\nline\",",
        "  \"last\": false",
        "}}",
    ],
}


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


def code_note():
    """Code.md as bytes, and its counts: headings over fenced code blocks."""
    lines = ["# Code stress note", ""]
    headings, blocks = 1, 0
    languages = list(CODE_SAMPLES)
    i = 0
    while True:
        language = languages[i % len(languages)]
        code = [line.format(i=i) for line in CODE_SAMPLES[language]]
        section = [f"## Section {i} ({language})", "", f"Paragraph {i} explains the {language} sample below.", "",
                   f"```{language}", *code, "```", ""]
        if len(lines) + len(section) > LINES:
            break
        lines += section
        headings, blocks, i = headings + 1, blocks + 1, i + 1
    lines += [f"Closing line {n}." for n in range(LINES - len(lines))]
    data = ("\n".join(lines) + "\n").encode()
    return data, {"lines": len(lines), "headings": headings, "code_blocks": blocks, "bytes": len(data)}


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
    (vault / SMALL).write_text(SMALL_TEXT)
    code, _ = code_note()
    code_digest = hashlib.sha256(code).hexdigest()
    if check and code_digest != CODE_NOTE_SHA256:
        raise SystemExit(f"{CODE_NOTE} drifted: sha256 {code_digest}, pinned {CODE_NOTE_SHA256}")
    (vault / CODE_NOTE).write_bytes(code)
    counts["sha256"] = digest
    return path, counts


if __name__ == "__main__":
    if len(sys.argv) not in (2, 3) or sys.argv[2:] not in ([], ["--check"]):
        raise SystemExit(__doc__)
    print(write(sys.argv[1], check=sys.argv[2:] == ["--check"])[1])
