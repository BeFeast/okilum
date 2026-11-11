#!/usr/bin/env python3
"""Generate the synthetic #2821 acceptance vault; refuse to overwrite files."""
import argparse
import base64
import json
from pathlib import Path
import tempfile


def populate(root):
    root.mkdir(parents=True, exist_ok=True)
    if any(root.iterdir()):
        raise ValueError("Choose an empty synthetic fixture directory")
    manifest = json.loads((Path(__file__).resolve().parents[2] / "tests/fixtures/at-vault.json").read_text())
    with tempfile.NamedTemporaryFile(prefix="CaseProbe-", dir=root) as probe:
        case_sensitive = not (root / Path(probe.name).name.upper()).exists()
    files = {name: text.encode() for name, text in manifest["text"].items()}
    files.update({name: base64.b64decode(text) for name, text in manifest["base64"].items()})
    if case_sensitive:
        files.update({name: text.encode() for name, text in manifest["case_sensitive"].items()})
    for name, content in files.items():
        target = root / name
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open("xb") as output:
            output.write(content)
    print(json.dumps({"fixture": str(root.resolve()), "case_sensitive": case_sensitive,
                      "twice": "included" if case_sensitive else "skipped: case-insensitive filesystem",
                      "files": len(files)}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    populate(parser.parse_args().destination)
