#!/usr/bin/env python3
"""Require native data-view inventory, runtime regressions and a fired control."""
import pathlib
import subprocess


def run(*args):
    result = subprocess.run(args, text=True, encoding="utf-8", stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    print(result.stdout, flush=True)
    return result


base = ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum']
listing = run(*base, 'data_view_2669_', '--', '--list')
assert listing.returncode == 0
names = [line for line in listing.stdout.splitlines() if line.endswith(': test')]
assert len(names) == 8, names
assert run(*base, 'data_view_2669_', '--', '--nocapture', '--test-threads=1').returncode == 0
table_inventory = run(*base, 'data_table_2735_', '--', '--list')
assert table_inventory.returncode == 0
assert len([line for line in table_inventory.stdout.splitlines() if line.endswith(': test')]) == 2
assert run(*base, 'data_table_2735_', '--', '--nocapture', '--test-threads=1').returncode == 0
table_source = pathlib.Path('crates/okilum-shell/src/reader_delimited.rs')
table_positive = table_source.read_bytes()
sort_action = b'this.sort_column(column, cx);'
assert table_positive.count(sort_action) == 1
try:
    table_source.write_bytes(table_positive.replace(sort_action, b'let _ = column;'))
    negative = run(*base, 'data_table_2735_native_sort_filter_copy_and_wide_virtualization', '--', '--nocapture', '--test-threads=1')
    assert negative.returncode != 0
    assert 'test result: FAILED' in negative.stdout and 'native header click sorted numerically' in negative.stdout
    print('DATA_TABLE_2735_COMPILED_DISABLED_SORT_CONTROL_FIRED', flush=True)
finally:
    table_source.write_bytes(table_positive)
assert run(*base, 'data_table_2735_', '--', '--nocapture', '--test-threads=1').returncode == 0
print('DATA_TABLE_2735_RESTORED_NATIVE_SUITE_PASS', flush=True)
# Preserve the real selection/edit/recovery/image contracts exercised by full Linux.
for regression in [
    'reader_viewer_chrome::',
    'reader_shortcuts::tests::markdown_copy_shortcut_has_no_reader_or_toolkit_collision',
    'reader_status_line::',
    'reader_editor::mode_navigation_tests::',
    'reader_loading::quick_view_tests::window_mode_switch_keeps_typed_preview_entities_in_both_directions',
    'reader_files::tests::delimited_attachment_edit_saves_only_changed_bytes_and_conflicts_keep_drafts',
    'reader_files::live::native_tests::native_image_notifications_evict_cached_pixels_and_recover',
]:
    inventory = run(*base, regression, '--', '--list')
    assert inventory.returncode == 0 and any(line.endswith(': test') for line in inventory.stdout.splitlines()), regression
    assert run(*base, regression, '--', '--nocapture', '--test-threads=1').returncode == 0, regression
source = pathlib.Path('crates/okilum-shell/src/reader_code_file.rs')
positive = source.read_bytes()
assert positive.count(b'.folding(true)') == 1
try:
    source.write_bytes(positive.replace(b'.folding(true)', b'.folding(false)'))
    # A compile failure or a missing test must never count as the control firing.
    negative = run(*base, 'data_view_2669_native_fold_copy_find_and_readonly_source', '--', '--nocapture', '--test-threads=1')
    assert negative.returncode != 0
    assert 'test result: FAILED' in negative.stdout and 'native XML grammar supplies folds' in negative.stdout
    print('DATA_VIEW_2669_COMPILED_DISABLED_FOLD_CONTROL_FIRED', flush=True)
finally:
    source.write_bytes(positive)
assert run(*base, 'data_view_2669_', '--', '--nocapture', '--test-threads=1').returncode == 0
print('DATA_VIEW_2669_RESTORED_NATIVE_SUITE_PASS', flush=True)
