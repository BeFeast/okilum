#!/usr/bin/env python3
"""Deterministic Markdown for the editor latency gate (#1639).

Use original generated prose instead of downloading Moby Dick: no network,
copyrighted corpus, random state, or host-dependent inputs are needed. Fixed
sentences rotate by paragraph index, are cropped to length, and wrap at 96
columns. ASCII text makes UTF-8 byte and character counts identical. Sections
include the initial heading; each note has one opening ``edit-marker``.

tiny.md is 3 KiB; sections-460.md is 130,000 bytes with exactly 460 sections;
one-mib.md is exactly 1,048,576 bytes; novel-1200k.md is 1,200,000 characters.
Run ``fixtures.py OUTDIR --check`` to refuse pinned-manifest drift before writing
the four notes and manifest.json. Omitting --check permits intentional repinning.
"""

import argparse
import hashlib
import json
import re
import textwrap
from pathlib import Path


HERE = Path(__file__).resolve().parent
SENTENCES = (
    "The river carried the morning light past the old stone bridge and into the quiet harbor.",
    "Mara opened her notebook and recorded the small changes that another traveler might overlook.",
    "Beyond the garden wall, a narrow path followed the fields toward a village among the hills.",
    "The keeper set a lantern beside the window while the last boat returned from the eastern shore.",
    "They compared their maps at the kitchen table and agreed to leave after the rain had passed.",
    "Every journey left a different story in the ledger, though the road itself seemed unchanged.",
    "At noon the clouds lifted, revealing a line of distant trees above the weathered roofs.",
    "She read the letter once more before folding it carefully and returning to the unfinished work.",
)
MARKDOWN = (
    "- Observe the river.\n- [ ] Record the next crossing.\n\n",
    "> The harbor keeps a different rhythm after sunset.\n\n",
    "| Place | State |\n| --- | --- |\n| Harbor | Quiet |\n\n",
    "```text\nroute: river -> harbor\npace: steady\n```\n\n",
    "A **careful record** keeps *small details* beside `field notes`.\n\n",
    "Read the [travel notes](tiny.md) and [[Harbor journal]] together.\n\n",
)
OPENING = "The edit-marker identifies this opening passage for the editor latency probe. "


def _paragraph(index: int, length: int, opening: bool = False) -> str:
    sentences = SENTENCES[index % len(SENTENCES):] + SENTENCES[:index % len(SENTENCES)]
    cycle = " ".join(sentences) + " "
    prefix = OPENING if opening else ""
    source = prefix + cycle * (length // len(cycle) + 1)
    # Wrapping replaces single spaces with LF, preserving the exact ASCII size.
    return textwrap.fill(source[:length - 1] + ".", width=96)


def _document(title: str, characters: int, sections: int) -> bytes:
    chunks = []
    section_size, extra = divmod(characters, sections)
    for index in range(sections):
        heading = f"# {title}\n\n" if index == 0 else f"## Passage {index + 1:04d}\n\n"
        markdown = MARKDOWN[index % len(MARKDOWN)]
        budget = section_size + (index < extra) - len(heading) - len(markdown)
        paragraphs = max(1, budget // 250)
        paragraph_size, remainder = divmod(budget - 2 * paragraphs, paragraphs)
        body = "".join(
            _paragraph(index * 7 + offset, paragraph_size + (offset < remainder),
                       opening=index == 0 and offset == 0) + "\n\n"
            for offset in range(paragraphs)
        )
        chunks.append(heading + body + markdown)
    return "".join(chunks).encode("utf-8")


def notes() -> dict[str, bytes]:
    """Return the complete, stable source bytes; no files or global state change."""
    return {
        "tiny.md": _document("A short river journal", 3_072, 4),
        "sections-460.md": _document("The journal in 460 sections", 130_000, 460),
        "one-mib.md": _document("One mebibyte of field notes", 1_048_576, 1_024),
        "novel-1200k.md": _document("The river keeper: a generated novel", 1_200_000, 1_200),
    }


def _manifest(files: dict[str, bytes]) -> dict[str, dict[str, int | str]]:
    result = {}
    for name, data in files.items():
        source = data.decode("utf-8")
        result[name] = {
            "sha256": hashlib.sha256(data).hexdigest(),
            "bytes": len(data),
            "characters": len(source),
            "sections": len(re.findall(r"^#{1,6} ", source, flags=re.MULTILINE)),
        }
    return result


def manifest() -> dict[str, dict[str, int | str]]:
    """Describe the generated notes, including all ATX heading sections."""
    return _manifest(notes())


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("outdir", type=Path)
    parser.add_argument("--check", action="store_true", help="refuse pinned-manifest drift before writing")
    args = parser.parse_args()
    files = notes()
    generated = _manifest(files)
    if args.check:
        pinned = HERE / "manifest.json"
        try:
            expected = json.loads(pinned.read_text(encoding="utf-8"))
        except (OSError, ValueError) as error:
            parser.exit(1, f"Cannot read pinned manifest {pinned}: {error}\n")
        if generated != expected:
            parser.exit(1, f"Fixtures differ from {pinned}; no output written.\n")

    args.outdir.mkdir(parents=True, exist_ok=True)
    for name, data in files.items():
        (args.outdir / name).write_bytes(data)
    encoded = (json.dumps(generated, indent=2, sort_keys=True) + "\n").encode("utf-8")
    (args.outdir / "manifest.json").write_bytes(encoded)
    for name, info in generated.items():
        print(f"{name}: {info['bytes']} bytes, {info['characters']} characters, {info['sections']} sections")


if __name__ == "__main__":
    main()
