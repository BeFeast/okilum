#!/usr/bin/env python3
"""#2988: mounted key dispatch with compiled regressions for both reported errors."""
from pathlib import Path
import re
import subprocess

BASE = ["cargo", "test", "--locked", "-p", "okilum-shell", "--bin", "okilum"]
PREFIX = "reader_tree_preview::keyboard_tests::"
RETURN = PREFIX + "issue2988_return_opens_focused_attachments_and_retains_note_folder_rename"
PREVIEW = PREFIX + "issue2988_system_quick_look_refusal_falls_back_and_arrows_preview_without_navigation"


def run(filter_name, expected, *, fails=False, exact=False):
    command = [*BASE, filter_name, "--"]
    if exact:
        command.append("--exact")
    result = subprocess.run([*command, "--nocapture", "--test-threads=1"],
                            text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    print(result.stdout, flush=True)
    if fails:
        assert result.returncode != 0 and "0 passed; 1 failed; 0 ignored;" in result.stdout
        assert f"test {filter_name} ..." in result.stdout, "negative control must run the target"
    else:
        assert result.returncode == 0
        footer = re.search(r"test result: ok\. (\d+) passed; 0 failed; 0 ignored;", result.stdout)
        assert footer and int(footer[1]) == expected, "require every selected case to execute"


def negative(source, broken, case):
    original = source.read_bytes()
    assert broken != original
    try:
        source.write_bytes(broken)
        run(case, 1, fails=True, exact=True)
    finally:
        source.write_bytes(original)
    run(case, 1, exact=True)
    assert source.read_bytes() == original


def main():
    run(PREFIX, 2)
    source = Path("crates/okilum-shell/src/main.rs")
    original = source.read_bytes()
    binding = b'KeyBinding::new("enter", TreeReturn, Some("ReaderTree && !Input"))'
    assert original.count(binding) == 1
    negative(source, original.replace(binding, binding.replace(b"TreeReturn", b"RenameTreeNote")), RETURN)

    source = Path("crates/okilum-shell/src/reader_tree_preview.rs")
    original = source.read_bytes()
    start = original.index(b"            let native = match self.tree_preview.show_native(&path) {")
    end = original.index(b"            if !native {", start)
    # The old macOS path propagated controller refusal to the toast and closed
    # the session. Restore that behavior while retaining the actual provider seam.
    broken = original[:start] + b"            let native = self.tree_preview.show_native(&path)?;\n" + original[end:]
    negative(source, broken, PREVIEW)

    run(PREFIX, 2)
    run("reader_tree_preview::tests::space_previews_selected_files_and_arrows_keep_compact_tree_focus", 1, exact=True)
    for case in ["inline_names_receive_tree_keys_and_same_name_is_noop",
                 "folder_keys_rename_without_toggling_and_move_updates_descendant_links"]:
        run("reader_move::tests::" + case, 1, exact=True)


if __name__ == "__main__":
    main()
