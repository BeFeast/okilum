#!/usr/bin/env python3
"""Mechanical Tessera -> Okilum rename (#970, stage 0 rebrand).

Run from a clean checkout of the exact main to rename. Deterministic and
idempotent: a second run changes nothing. Writes a residual report.

    python3 scripts/rebrand/okilum_rename.py [--dry-run] [--report PATH]

What changes: crate/package/module/binary names, paths, imports, display name,
bundle ID (com.befeast.okilum), Windows packId (BeFeast.Okilum), repository
(BeFeast/okilum), env var names (OKILUM_*), workflows, scripts and current docs.

Since #987 nothing stays for compatibility: schema ids, hash namespaces,
internal URLs, fixtures and experiments are renamed too (Okilum started from
clean folders; its own early state resets once). Only this detector directory,
vendor/ and the verbatim third-party licence texts are left as they are.
"""
import argparse
import collections
import os
import re
import subprocess
import sys

EXCLUDED_PREFIXES = ("scripts/rebrand/", "vendor/")
# Third-party licence texts stay verbatim. Our own text under licenses/
# (README.md, the Foxit-PDFium.txt preface) is renamed like the rest.
EXCLUDED_FILES = (
    "licenses/Apache-2.0.txt", "licenses/Lucide.txt", "licenses/Sparkle.txt",
    "licenses/excalifont-provenance.json",
)
# Brand assets are pinned by a hash manifest to the brand repository; the
# Okilum symbol arrives as its own import (scripts/brand-assets.py), not as text edits.
EXCLUDED_PARTS = ("/assets/brand/",)
BINARY_SUFFIXES = (".png", ".ico", ".icns", ".ttf", ".woff", ".woff2", ".zip", ".gz", ".pdf", ".jpg")

# Ordered: specific identities first, generic case forms last.
REPLACEMENTS = [
    ("BeFeast/tessera", "BeFeast/okilum"),
    ("uk.oklabs.tessera", "com.befeast.okilum"),
    ("BeFeast.Tessera", "BeFeast.Okilum"),
    ("TESSERA", "OKILUM"),
    ("Tessera", "Okilum"),
    ("tessera", "okilum"),
]
RUNTIME_ENV_RE = re.compile(r"""env::var(?:_os)?\(\s*"(OKILUM_[A-Z0-9_]+)"\s*\)""")


def git(*args):
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout


def excluded(path):
    """Contents are left as they are (history, fixtures, vendored diffs)."""
    return path in EXCLUDED_FILES or path.startswith(EXCLUDED_PREFIXES) or any(p in f"/{path}" for p in EXCLUDED_PARTS)


def frozen_path(path):
    """Paths kept as they are. Fixtures inside renamed crates still move with them."""
    return path in EXCLUDED_FILES or path.startswith(EXCLUDED_PREFIXES)


def rename_text(text):
    out = []
    for line in text.splitlines(keepends=True):
        for old, new in REPLACEMENTS:
            line = line.replace(old, new)
        out.append(line)
    return "".join(out)


def rename_path(path):
    parts = path.split("/")
    return "/".join(rename_text(p) if "tessera" in p.lower() else p for p in parts)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--report", default="rebrand-report.md")
    args = parser.parse_args()
    if git("status", "--porcelain").strip() and not args.dry_run:
        sys.exit("working tree is not clean")
    files = [f for f in git("ls-files", "-z").split("\0") if f]
    changed, moved = [], []
    for path in files:
        if excluded(path) or path.endswith(BINARY_SUFFIXES) or os.path.islink(path):
            continue
        try:
            with open(path, encoding="utf-8", newline="") as handle:
                text = handle.read()
        except (UnicodeDecodeError, IsADirectoryError, FileNotFoundError):
            continue
        new = rename_text(text)
        if new != text:
            changed.append(path)
            if not args.dry_run:
                with open(path, "w", encoding="utf-8", newline="") as handle:
                    handle.write(new)
    # Paths after contents, deepest first, so parent moves carry renamed children.
    for path in sorted(files, key=lambda p: -p.count("/")):
        if frozen_path(path):
            continue
        target = rename_path(path)
        if target != path:
            moved.append((path, target))
            if not args.dry_run:
                os.makedirs(os.path.dirname(target) or ".", exist_ok=True)
                git("mv", "-k", path, target)
    # `use okilum_…` sorts differently from `use tessera_…`; keep rustfmt clean.
    # rustfmt follows `#[path]` modules into vendor/: run scripts/vendor-setup.sh first.
    # Only workspace crates are formatted; experiments keep snapshots that do not build alone.
    rust = [rename_path(f) for f in changed if f.endswith(".rs") and f.startswith("crates/")]
    unformatted = []
    if not args.dry_run:
        for path in rust:
            if subprocess.run(["rustfmt", "--edition", "2021", path], capture_output=True).returncode:
                unformatted.append(path)
    if unformatted:
        sys.exit("rustfmt failed (is vendor/ set up?): " + ", ".join(unformatted))
    # Residuals: what still says tessera, by reason.
    residual = collections.Counter()
    examples = collections.defaultdict(set)
    env_reads = set()
    for path in [f if frozen_path(f) else rename_path(f) for f in files]:
        if not os.path.isfile(path) or path.endswith(BINARY_SUFFIXES):
            continue
        try:
            text = open(path, encoding="utf-8").read()
        except UnicodeDecodeError:
            continue
        env_reads.update(RUNTIME_ENV_RE.findall(text))
        for line in text.splitlines():
            if "tessera" not in line.lower():
                continue
            if excluded(path):
                reason = "excluded tree: " + next(p for p in EXCLUDED_FILES + EXCLUDED_PREFIXES + EXCLUDED_PARTS if p.strip("/") in f"/{path}")
            else:
                reason = "UNEXPECTED"
            residual[reason] += 1
            if len(examples[reason]) < 3:
                examples[reason].add(f"{path}: {line.strip()[:120]}")
    with open(args.report, "w") as report:
        report.write(f"# Okilum rename report\n\n- files changed: {len(changed)}\n- paths moved: {len(moved)}\n\n")
        report.write("## Residual `tessera` lines by reason\n\n")
        for reason, count in residual.most_common():
            report.write(f"- **{reason}**: {count}\n")
            for example in sorted(examples[reason]):
                report.write(f"  - `{example}`\n")
        report.write("\n## Runtime env reads that need a legacy `TESSERA_*` fallback (#967)\n\n")
        for name in sorted(env_reads):
            report.write(f"- `{name}`\n")
    print(f"changed {len(changed)} files, moved {len(moved)} paths, unexpected residual lines: {residual.get('UNEXPECTED', 0)}")
    return 1 if residual.get("UNEXPECTED") else 0


if __name__ == "__main__":
    sys.exit(main())
