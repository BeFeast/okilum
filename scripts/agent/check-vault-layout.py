#!/usr/bin/env python3
"""Reject legacy vault folder literals in the core and desktop Rust sources."""

from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
CRATES = ("okilum-core", "okilum-shell")

# Fixtures live under tests/; inline #[cfg(test)] modules are skipped below.
# Exact non-vault strings, keyed by filename.
ALLOW = {
    "open_in.rs": {
        "Contents/Resources/app/bin/cursor",
        "Contents/Resources/app/bin/code",
        "Contents/Resources/app/bin/code-insiders",
        "Contents/Resources/app/bin/windsurf",
    },
    "macos.rs": {"Contents/Resources/app/bin/code"},
    "sprint.rs": {"Resources", "Resources/sprint-context.json"},
    "build.rs": {"resources/windows/okilum.rc", "cargo:rerun-if-changed=resources/windows/okilum.rc"},
    "quick_open.rs": {"Projects", "projects"},
    "reader_shortcuts.rs": {"Projects"},
    "reader_title_nav.rs": {"Projects"},
    "reader_window_view.rs": {"Projects"},
    "reader_workbench.rs": {"Areas"},
    "brain/maestro_ui.rs": {"projects"},
    "connectors.rs": {"projects"},
    "daily_notes.rs": {"daily", "daily notes"},
    "kubernetes_workspace/harness.rs": {"resources"},
    "kubernetes_workspace/inspector.rs": {"RESOURCES"},
    "reader_project_navigation.rs": {"Projects"},
    "records/note.rs": {"archive", "daily"},
    "view/builtin/mod.rs": {"ops/daily"},
}

SEGMENT = r"(?:Projects|Areas|Resources|(?:\d+[ .-]+)?Archive|Архив|Clippings|Daily(?: Notes)?|[@_]Assets)"
PATH = re.compile(rf"(?:^|[/\\]){SEGMENT}(?:[/\\]|$)|\s{SEGMENT}[/\\]", re.IGNORECASE)
ASSET = re.compile(r"[@_]Assets\b", re.IGNORECASE)
LEX = re.compile(r'(?P<comment>//[^\n]*|/\*.*?\*/)|'
                 r'(?P<raw>r(?P<hash>\#*)".*?"(?P=hash))|'
                 r'(?P<string>"(?:\\.|[^"\\])*")|'
                 r"'(?:\\(?:u\{[^}]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'|"
                 r'(?P<test>\#\s*\[\s*cfg\s*\(\s*(?:test|all\(\s*test\s*,[^\n]*\))\s*\)\s*\])|'
                 r'(?P<brace>[{};])', re.DOTALL)


def rust_strings(source):
    """Yield string literals outside test-only items, with line numbers."""
    test_depth = None
    depth = 0
    for token in LEX.finditer(source):
        if token.group('test') and test_depth is None:
            test_depth = depth
        elif token.group('brace') == "{":
            depth += 1
        elif token.group('brace') == "}":
            depth -= 1
            if depth == test_depth:
                test_depth = None
        elif token.group('brace') == ";" and depth == test_depth:
            test_depth = None
        elif test_depth is None:
            if raw := token.group('raw'):
                offset = 2 + len(token.group('hash'))
                value = raw[offset:-(offset - 1)]
            elif string := token.group('string'):
                value = string[1:-1]
            else:
                continue
            yield source.count("\n", 0, token.start()) + 1, value


def main():
    matches = 0
    for crate in CRATES:
        directory = ROOT / "crates" / crate
        for file in sorted([*(directory / "src").rglob("*.rs"), *directory.glob("build.rs")]):
            relative = file.relative_to(ROOT).as_posix()
            if ("tests" in file.parts or file.stem == "tests"
                    or file.stem.endswith(("_tests", "_test"))):
                continue
            allowed = {
                value
                for name, values in ALLOW.items() if relative.endswith("/" + name)
                for value in values
            }
            for line, value in rust_strings(file.read_text()):
                if value not in allowed and (PATH.search(value) or ASSET.search(value)):
                    print(f"{relative}:{line}: {value!r}")
                    matches += 1
    return bool(matches)


if __name__ == "__main__":
    sys.exit(main())
