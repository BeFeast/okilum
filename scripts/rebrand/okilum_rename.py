#!/usr/bin/env python3
"""Mechanical Tessera -> Okilum rename (#970, stage 0 rebrand).

Run from a clean checkout of the exact main to rename. Deterministic and
idempotent: a second run changes nothing. Writes a residual report.

    python3 scripts/rebrand/okilum_rename.py [--dry-run] [--report PATH]

What changes: crate/package/module/binary names, paths, imports, display name,
bundle ID (com.befeast.okilum), Windows packId (BeFeast.Okilum), repository
(BeFeast/okilum), env var names (OKILUM_*), workflows, scripts and current docs.

What stays (legacy compatibility, see docs/rebrand-okilum.md):
- persisted schema ids and namespaces (`tessera-…/vN`, `tessera/…/vN`): changing
  them changes readers and deterministic ids;
- recovery/file markers (`.tessera-save-…`, `.tessera-index`, …) and URL forms
  (`tessera://`, `tessera-asset://`): new names come from the compatibility layer
  (#967), which reads both;
- sync identities (launchd label, Windows task/pipe, systemd units) and the
  Sparkle beta preference key: migrated by their own owners, not by renaming;
- any line containing `rebrand: keep` (the #967 legacy constants);
- historical and fixture trees listed in EXCLUDED_PREFIXES.
"""
import argparse
import collections
import os
import re
import subprocess
import sys

EXCLUDED_PREFIXES = (
    "docs/archive/", "docs/research/", "docs/upstream/", "experiments/", "fixtures/",
    "scripts/patches/", "scripts/rebrand/", "vendor/", "licenses/",
    "docs/rebrand-okilum.md",
)
# Brand assets are pinned by a hash manifest to the brand repository; the
# Okilum symbol arrives as its own import (scripts/brand-assets.py), not as text edits.
EXCLUDED_PARTS = ("/tests/fixtures/", "/testdata/", "/assets/brand/")
BINARY_SUFFIXES = (".png", ".ico", ".icns", ".ttf", ".woff", ".woff2", ".zip", ".gz", ".pdf", ".jpg")

# Kept verbatim. Order does not matter; each match is shielded before replacing.
PROTECTED = [
    r"tessera-[a-z0-9-]+/v[0-9]+",                 # persisted schema ids
    r"tessera/[a-z0-9-]+(?:/[a-z0-9-]+)*/v[0-9]+",  # deterministic namespaces
    r"\.tessera-(?:save|source|unit|index)[A-Za-z0-9_.*-]*",  # file/recovery markers
    r"tessera(?:-asset)?://",                      # URL forms, dual-read by #967
    r"Tessera-Sync-[A-Za-z0-9_{}<>.-]*",           # Windows task / pipe names
    r"tessera-syncthing-[A-Za-z0-9_{}<>.-]*",      # Linux sync units
    r"uk\.oklabs\.tessera\.sync[A-Za-z0-9_.]*",    # launchd sync label
    r"TesseraReceiveBetaBuilds",                   # Sparkle beta preference key
]
PROTECTED_RE = re.compile("|".join(f"(?:{p})" for p in PROTECTED))

# Ordered: specific identities first, generic case forms last.
REPLACEMENTS = [
    ("BeFeast/tessera", "BeFeast/okilum"),
    ("uk.oklabs.tessera", "com.befeast.okilum"),
    ("BeFeast.Tessera", "BeFeast.Okilum"),
    ("TESSERA", "OKILUM"),
    ("Tessera", "Okilum"),
    ("tessera", "okilum"),
]
KEEP_MARKER = "rebrand: keep"
RUNTIME_ENV_RE = re.compile(r"""env::var(?:_os)?\(\s*"(OKILUM_[A-Z0-9_]+)"\s*\)""")


def git(*args):
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout


def excluded(path):
    """Contents are left as they are (history, fixtures, vendored diffs)."""
    return path.startswith(EXCLUDED_PREFIXES) or any(p in f"/{path}" for p in EXCLUDED_PARTS)


def frozen_path(path):
    """Paths kept as they are. Fixtures inside renamed crates still move with them."""
    return path.startswith(EXCLUDED_PREFIXES)


def rename_text(text):
    out = []
    for line in text.splitlines(keepends=True):
        if KEEP_MARKER in line:
            out.append(line)
            continue
        pieces, last = [], 0
        for m in PROTECTED_RE.finditer(line):
            pieces.append(("edit", line[last:m.start()]))
            pieces.append(("keep", m.group(0)))
            last = m.end()
        pieces.append(("edit", line[last:]))
        rebuilt = []
        for kind, chunk in pieces:
            if kind == "edit":
                for old, new in REPLACEMENTS:
                    chunk = chunk.replace(old, new)
            rebuilt.append(chunk)
        out.append("".join(rebuilt))
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
    rust = [rename_path(f) for f in changed if f.endswith(".rs")]
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
                reason = "excluded tree: " + next(p for p in EXCLUDED_PREFIXES + EXCLUDED_PARTS if p.strip("/") in f"/{path}")
            elif KEEP_MARKER in line:
                reason = "rebrand: keep"
            elif PROTECTED_RE.search(line):
                reason = "protected: " + PROTECTED_RE.search(line).group(0)
                reason = re.sub(r"[0-9a-f]{6,}|/v[0-9]+$", "", reason)[:60]
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
