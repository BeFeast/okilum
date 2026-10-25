#!/usr/bin/env python3
"""Bundle the complete Lucide icon set for the project icon picker (#1627).

Reads an unpacked lucide-static npm package (pinned to the version T3 Code
ships, see LUCIDE_VERSION) and writes one gzip'd text file:

    # header
    <name>\t<svg body>\t<tag,tag,...>     a Lucide icon
    <alias>\t=<name>                        a Lucide alias (T3 accepts both)

The body is the SVG's inner elements; every Lucide icon shares the same
24x24 stroke header, which okilum-shell's `lucide` module adds back.

    curl -fsSLO https://registry.npmjs.org/lucide-static/-/lucide-static-0.564.0.tgz
    tar xzf lucide-static-0.564.0.tgz          # in an empty directory
    python3 -I scripts/lucide/bundle.py <dir>/package crates/okilum-shell/assets/lucide/lucide.tsv.gz
"""
import gzip
import json
import os
import re
import sys

LUCIDE_VERSION = "0.564.0"
HEADER = (
    'xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" '
    'fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" '
    'stroke-linejoin="round"'
)


def body(svg: str, name: str) -> str:
    match = re.search(r"<svg([^>]*)>(.*)</svg>", svg, re.S)
    if not match:
        sys.exit(f"{name}: no <svg> element")
    attributes = re.sub(r'\s*class="[^"]*"', "", match.group(1))
    if " ".join(attributes.split()) != HEADER:
        sys.exit(f"{name}: unexpected svg attributes {attributes!r}")
    inner = re.sub(r"\s+", " ", match.group(2)).strip()
    inner = re.sub(r">\s+<", "><", inner).replace(" />", "/>")
    if "\t" in inner or "\n" in inner:
        sys.exit(f"{name}: tab or newline in body")
    return inner


def main() -> None:
    package, output = sys.argv[1], sys.argv[2]
    meta = json.load(open(os.path.join(package, "package.json"), encoding="utf-8"))
    if meta.get("version") != LUCIDE_VERSION:
        sys.exit(f"expected lucide-static {LUCIDE_VERSION}, got {meta.get('version')}")
    tags = json.load(open(os.path.join(package, "tags.json"), encoding="utf-8"))
    icons = os.path.join(package, "icons")
    bodies = {}
    for file in sorted(os.listdir(icons)):
        if file.endswith(".svg"):
            name = file[:-4]
            bodies[name] = body(open(os.path.join(icons, file), encoding="utf-8").read(), name)
    by_body = {bodies[name]: name for name in sorted(tags)}
    lines = [f"# lucide-static {LUCIDE_VERSION} (ISC), {len(tags)} icons, aliases follow"]
    for name in sorted(tags):
        clean = [t.replace(",", " ").replace("\t", " ").strip() for t in tags[name]]
        lines.append(f"{name}\t{bodies[name]}\t{','.join(t for t in clean if t)}")
    aliases = 0
    for name in sorted(bodies):
        if name in tags:
            continue
        target = by_body.get(bodies[name])
        if target is None:
            sys.exit(f"{name}: not an icon and not an alias of one")
        lines.append(f"{name}\t={target}")
        aliases += 1
    data = ("\n".join(lines) + "\n").encode("utf-8")
    with open(output, "wb") as out:
        out.write(gzip.compress(data, compresslevel=9, mtime=0))
    print(f"{len(tags)} icons, {aliases} aliases, {len(data)} bytes, {os.path.getsize(output)} gzip'd")


if __name__ == "__main__":
    main()
