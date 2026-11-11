"""Require a nonempty parent libtest verdict from the selected Cargo target."""
from pathlib import Path
import re
import sys

ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
RUNNING = re.compile(r"^Running (?:unittests |tests/).*\((.+)\)$")
SUMMARY = re.compile(r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed;")


def passed(text, target):
    current = None
    footer = None
    verdicts = []
    for raw in text.splitlines():
        line = ANSI.sub('', raw).strip()
        match = RUNNING.match(line)
        if match or line.startswith('Doc-tests '):
            if current == target:
                verdicts.append(footer)
            current = Path(match[1]).name.rsplit('-', 1)[0] if match else None
            footer = None
        summary = SUMMARY.match(line)
        if summary:
            # Subprocess fixtures can print an earlier positive footer. Only
            # the final parent footer before the next Cargo target counts.
            footer = summary[1] == 'ok' and int(summary[2]) > 0 and int(summary[3]) == 0
    if current == target:
        verdicts.append(footer)
    return bool(verdicts) and all(verdicts)


if __name__ == '__main__':
    target, receipt = sys.argv[1:]
    if not passed(Path(receipt).read_text(), target):
        print(f'No non-zero passing test count for required target {target}', file=sys.stderr)
        raise SystemExit(1)
