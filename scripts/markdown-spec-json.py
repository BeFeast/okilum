#!/usr/bin/env python3
"""Extract spec examples from a CommonMark-style spec.txt into JSON (#650).

The CommonMark project publishes spec.json for every release; the GFM spec
(github/cmark-gfm test/spec.txt) does not. This mirrors cmark-gfm's
test/spec_tests.py `--dump-tests` so the vendored GFM JSON is reproducible:

    python3 scripts/markdown-spec-json.py spec.txt > gfm-0.29.json

Examples marked `disabled` are kept with that tag, unlike upstream, so the
harness can count and report them instead of silently losing them.
"""
import json
import re
import sys

FENCE = "`" * 32


def examples(path):
    tests = []
    number = 0
    start = 0
    state = 0  # 0 prose, 1 markdown, 2 html
    markdown, html, extensions = [], [], []
    section = ""
    heading = re.compile(r"#+ ")
    with open(path, encoding="utf-8", newline="\n") as spec:
        for line_number, line in enumerate(spec, 1):
            stripped = line.strip()
            if stripped.startswith(FENCE + " example"):
                state = 1
                extensions = stripped[len(FENCE + " example"):].split()
            elif stripped == FENCE:
                number += 1
                tests.append({
                    "markdown": "".join(markdown).replace("→", "\t"),
                    "html": "".join(html).replace("→", "\t"),
                    "example": number,
                    "start_line": start,
                    "end_line": line_number,
                    "section": section,
                    "extensions": extensions,
                })
                state, start, markdown, html = 0, 0, [], []
            elif stripped == "." and state == 1:
                state = 2
            elif state == 1:
                if start == 0:
                    start = line_number - 1
                markdown.append(line)
            elif state == 2:
                html.append(line)
            elif state == 0 and heading.match(line):
                section = heading.sub("", line, count=1).strip()
    return tests


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    json.dump(examples(sys.argv[1]), sys.stdout, indent=2, ensure_ascii=False)
    sys.stdout.write("\n")
