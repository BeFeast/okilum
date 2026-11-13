#!/usr/bin/env python3
"""Native inventory, full regressions and compiled runtime negative controls."""
import os
import pathlib
import subprocess

CONTROL = 'OKILUM_DATA_VIEW_NATIVE_CONTROL'
BASE = ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum', '--features', 'data-view-native-harness']


def run(*args, control=None):
    env = dict(os.environ)
    env.pop(CONTROL, None)
    if control:
        env[CONTROL] = control
    result = subprocess.run(args, env=env, text=True, encoding='utf-8',
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    print(result.stdout, flush=True)
    return result


def suite(filter_, expected=None):
    inventory = run(*BASE, filter_, '--', '--list')
    assert inventory.returncode == 0
    names = [line for line in inventory.stdout.splitlines() if line.endswith(': test')]
    assert names and (expected is None or len(names) == expected), (filter_, names)
    assert run(*BASE, filter_, '--', '--nocapture', '--test-threads=1').returncode == 0, filter_


# Compile the three controls together rather than rebuilding the large UI crate
# for each mutation. Every default-positive suite and fired negative uses that
# same instrumented binary. No compilation failure counts as a fired control.
paths = {name: pathlib.Path('crates/okilum-shell/src') / name for name in
         ['main.rs', 'reader_delimited.rs', 'reader_code_file.rs', 'reader_code_file/toolbar.rs']}
original = {name: path.read_bytes() for name, path in paths.items()}
mutations = {
    'reader_delimited.rs': (b'this.sort_column(column, cx);',
                           b'if !crate::data_view_native_control("sort") { this.sort_column(column, cx); }'),
    'reader_code_file.rs': (b'.folding(true)', b'.folding(!crate::data_view_native_control("fold"))'),
    'reader_code_file/toolbar.rs': (b'if self.view != View::Table || self.table.is_some() {',
                                  b'if (self.view != View::Table || self.table.is_some()) && !crate::data_view_native_control("find") {'),
}
try:
    for name, (before, after) in mutations.items():
        assert original[name].count(before) == 1, name
        paths[name].write_bytes(original[name].replace(before, after))
    paths['main.rs'].write_bytes(original['main.rs'] + b'''
#[cfg(test)]
fn data_view_native_control(name: &str) -> bool {
    std::env::var("OKILUM_DATA_VIEW_NATIVE_CONTROL").is_ok_and(|value| value == name)
}
''')
    suite('data_view_2669_', 8)
    suite('data_table_2735_', 2)
    # Keep the existing selection/edit/recovery/image coverage unchanged.
    for regression in [
        'file2811_',
        'edit2803_',
        'code2771_',
        'code2810_',
        'reader_viewer_chrome::',
        'reader_shortcuts::tests::markdown_copy_shortcut_has_no_reader_or_toolkit_collision',
        'reader_status_line::',
        'reader_editor::mode_navigation_tests::',
        'reader_loading::quick_view_tests::window_mode_switch_keeps_typed_preview_entities_in_both_directions',
        'reader_files::tests::delimited_attachment_edit_saves_only_changed_bytes_and_conflicts_keep_drafts',
        'reader_files::live::native_tests::native_image_notifications_evict_cached_pixels_and_recover',
    ]:
        suite(regression)
    for control, test, assertion, marker in [
        ('sort', 'data_table_2735_native_sort_filter_copy_and_wide_virtualization',
         'native header click sorted numerically', 'DATA_TABLE_2735_COMPILED_DISABLED_SORT_CONTROL_FIRED'),
        ('fold', 'data_view_2669_native_fold_copy_find_and_readonly_source',
         'native XML grammar supplies folds', 'DATA_VIEW_2669_COMPILED_DISABLED_FOLD_CONTROL_FIRED'),
        ('find', 'code2810_every_read_format_uses_header_field_and_unfolds_first_hit',
         'shared field', 'CODE2810_COMPILED_MISSING_HEADER_FIND_CONTROL_FIRED'),
    ]:
        negative = run(*BASE, test, '--', '--nocapture', '--test-threads=1', control=control)
        assert negative.returncode != 0
        assert 'running 1 test' in negative.stdout
        assert 'test result: FAILED' in negative.stdout and 'panicked at' in negative.stdout
        assert assertion in negative.stdout, (control, negative.stdout)
        print(marker, flush=True)
finally:
    for name, data in original.items():
        paths[name].write_bytes(data)
assert all(path.read_bytes() == original[name] for name, path in paths.items())
# Rebuild the actual restored source once, retaining all three restoration
# positives. The native app above was built before any test instrumentation.
for filter_, marker in [
    ('data_table_2735_', 'DATA_TABLE_2735_RESTORED_NATIVE_SUITE_PASS'),
    ('data_view_2669_', 'DATA_VIEW_2669_RESTORED_NATIVE_SUITE_PASS'),
    ('code2810_', 'CODE2810_RESTORED_NATIVE_SUITE_PASS'),
]:
    suite(filter_)
    print(marker, flush=True)
