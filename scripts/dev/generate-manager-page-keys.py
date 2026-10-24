#!/usr/bin/env python3
"""Regenerate the Objective-C string from the canonical page fallback helper."""
from pathlib import Path
source = Path(__file__).resolve().parents[2] / "crates/okilum-shell/src/reader_manager_focus/page_keys.js"
text = source.read_text()
escaped = text.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")
source.with_suffix(".inc").write_text('// Generated from page_keys.js; run scripts/dev/generate-manager-page-keys.py.\nstatic NSString *const KeyPageScript = @"' + escaped + '";\n')
