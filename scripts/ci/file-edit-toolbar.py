#!/usr/bin/env python3
"""Compile file Edit controls together, require positives and real failing tests.

Instrumentation exists only during this script. The native app is built from
restored production source by data-view-native.yml before these unit controls.
"""
import difflib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / 'target/file-edit-toolbar-controls'
OUT.mkdir(parents=True, exist_ok=True)
BASE = ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum',
        '--features', 'data-view-native-harness']
if '--release' in sys.argv:
    BASE.append('--release')
if '--linux-sprint' in sys.argv:
    BASE[BASE.index('data-view-native-harness')] = 'linux-sprint,data-view-native-harness'
if '--shell-no-opt' in sys.argv:
    BASE.extend(['--config', 'profile.release.package.okilum-shell.opt-level=0'])
ENV = {k: v for k, v in os.environ.items() if k != 'OKILUM_FILE2811_CONTROL'}
original = {}
patch = []


def edit(name, old, new, count=1):
    path = ROOT / ('crates/okilum-shell/src/' + name)
    data = path.read_bytes()
    source = data.decode()
    assert source.count(old) == count, (name, old, source.count(old))
    original.setdefault(path, data)
    changed = source.replace(old, new)
    path.write_bytes(changed.encode())
    patch.append(''.join(difflib.unified_diff(source.splitlines(True), changed.splitlines(True),
                                            fromfile=name, tofile=name)))


def run(label, args, control=None):
    print('FILE2811_PHASE', label, flush=True)
    env = dict(ENV)
    if control:
        env['OKILUM_FILE2811_CONTROL'] = control
    result = subprocess.run(args, cwd=ROOT, env=env, text=True, encoding='utf-8',
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (OUT / (label + '.log')).write_text(result.stdout)
    print(result.stdout[-5000:], flush=True)
    return result


def positive(binary, filter_, expected=None):
    result = run('positive-' + filter_.replace(':', '_'),
                 [binary, filter_, '--nocapture', '--test-threads=1'])
    assert result.returncode == 0 and re.search(r'test result: ok\. [1-9]\d* passed', result.stdout), filter_
    if expected is not None:
        assert re.search(rf'test result: ok\. {expected} passed; 0 failed;', result.stdout), filter_


CONTROLS = [
    ('geometry-partition', 'edit2803_native_geometry_accepts_same_font_run_batches'),
    ('missing-file-icons', 'brand::tests::every_local_functional_icon_is_embedded'),
    ('conflict-toast', 'file2811_conflict_bottom_actions_are_not_covered_by_a_duplicate_toast'),
    ('legacy-host', 'file2811_note_host_has_a_positive_control_and_file_keeps_one_header'),
    ('live-part-cycle', 'file2811_app_teardown_releases_live_parts_and_pending_json_validation'),
    ('missing-find', 'file2811_note_host_has_a_positive_control_and_file_keeps_one_header'),
    ('editing-view', 'file2811_note_host_has_a_positive_control_and_file_keeps_one_header'),
    ('editing-fold', 'file2811_note_host_has_a_positive_control_and_file_keeps_one_header'),
    ('query-header', 'file2811_header_find_and_typing_do_not_render_slots_or_notify_panels'),
    ('source-header', 'file2811_header_find_and_typing_do_not_render_slots_or_notify_panels'),
    ('source-panels', 'file2811_header_find_and_typing_do_not_render_slots_or_notify_panels'),
    ('live-json', 'file2811_live_json_badge_and_bottom_follow_one_revision_without_refresh'),
    ('json-debounce', 'file2811_live_json_coalesces_keys_and_cancels_an_inflight_revision'),
    ('json-cancel', 'file2811_live_json_coalesces_keys_and_cancels_an_inflight_revision'),
    ('json-badge', 'file2811_live_json_badge_and_bottom_follow_one_revision_without_refresh'),
    ('bottom-line', 'file2811_live_json_badge_and_bottom_follow_one_revision_without_refresh'),
    ('busy-enabled', 'file2811_format_menu_has_live_commands_language_and_busy_explanations'),
    ('failed-save-enabled', 'file2811_actual_save_failure_and_conflict_disable_all_commands'),
    ('conflict-enabled', 'file2811_actual_save_failure_and_conflict_disable_all_commands'),
    ('dead-validate-menu', 'file2811_format_menu_has_live_commands_language_and_busy_explanations'),
    ('flowing-sheet', 'file2811_repair_sheet_keeps_top_cancel_bytes_apply_and_undo'),
    ('cancel-mutates', 'file2811_repair_sheet_keeps_top_cancel_bytes_apply_and_undo'),
    ('apply-saves', 'file2811_repair_sheet_keeps_top_cancel_bytes_apply_and_undo'),
    ('read-repair', 'file2811_repair_from_read_enters_edit_and_stale_sheet_closes'),
    ('stale-conflict-sheet', 'file2811_conflicting_repair_closes_the_sheet_without_applying_or_writing'),
    ('lost-presentation', 'file2811_return_to_read_restores_view_folds_query_and_does_not_remember_raw'),
    ('lost-page', 'file2811_raw_parts_and_async_formatted_restore_after_edit'),
    ('dead-shortcuts', 'file2811_explicit_non_json_validation_refusal_and_shortcuts_use_the_new_host'),
    ('lost-toast', 'file2811_primary_format_preserves_tokens_bom_crlf_and_one_undo'),
    ('bom-body', 'file2811_primary_format_preserves_tokens_bom_crlf_and_one_undo'),
    ('bottom-wording', 'file2811_real_refusals_and_syntax_verdicts_have_one_bottom_home'),
    ('language-fallback', 'file2811_ambiguous_language_has_a_button_then_a_narrow_menu'),
]

try:
    edit('platform/shaped_line_cache.rs',
         '    fn same_native_geometry(a: &LineLayout, b: &LineLayout) -> bool {',
         '    fn same_native_geometry(a: &LineLayout, b: &LineLayout) -> bool {\n        if crate::file2811_control("geometry-partition") { return native_ascii::exact_geometry(a, b); }')
    edit('brand.rs', '        if let Some((_, bytes)) = IMAGES.iter().find(|(name, _)| *name == path) {',
         '        if crate::file2811_control("missing-file-icons") && matches!(path, "icons/indent.svg" | "icons/wrench.svg") { return Ok(None); }\n        if let Some((_, bytes)) = IMAGES.iter().find(|(name, _)| *name == path) {')
    edit('reader_toast.rs', '.filter(|notice| !self.file_conflict_in_status(notice))',
         '.filter(|notice| !self.file_conflict_in_status(notice) || crate::file2811_control("conflict-toast"))')
    edit('main.rs', 'fn main() {', '''#[cfg(test)]
fn file2811_control(name: &str) -> bool {
    std::env::var("OKILUM_FILE2811_CONTROL").is_ok_and(|v|v==name)
}
fn main() {''')
    base = 'reader_editor/code_operations/file_chrome.rs'
    edit('reader_editor/code_operations.rs', '.is_some_and(|editing| !editing.markdown)',
         '.is_some_and(|editing| !editing.markdown) && !crate::file2811_control("legacy-host")')
    edit(base, 'struct Part {', 'struct Part {\n    _hold: Option<Entity<FileChrome>>,')
    edit(base, 'file: file.downgrade(),', 'file: file.downgrade(),\n            _hold: crate::file2811_control("live-part-cycle").then_some(file.clone()),')
    edit(base, 'chrome.find = Some(find);', 'if !crate::file2811_control("missing-find") {chrome.find = Some(find);}')
    edit(base, 'action.enabled = *view == View::Raw;',
         'action.enabled = *view == View::Raw || crate::file2811_control("editing-view");')
    edit(base, 'fold.enabled = false;', 'fold.enabled = crate::file2811_control("editing-fold");')
    edit('reader_viewer_chrome.rs', 'reader.selected_title(),', '''reader.selected_title()+&if crate::file2811_control("query-header") || crate::file2811_control("source-header") {
reader.editing.as_ref().map(|e| if crate::file2811_control("source-header") {format!("{:?}", e.input().read(cx).source_stamp())} else { reader.file_editor_find_target(cx).map(|f|f.input.read(cx).value().to_string()).unwrap_or_default() }).unwrap_or_default()
} else {String::new()},''')
    edit('reader_editor.rs', 'if operation_changed && editing.markdown {',
         'if operation_changed && editing.markdown || crate::file2811_control("source-panels") {')
    edit(base, 'if file.json {\n                file.schedule(cx);', 'if file.json && !crate::file2811_control("live-json") {\n                file.schedule(cx);')
    edit(base, 'Duration::from_millis(120)', 'Duration::from_millis(if crate::file2811_control("json-debounce") {0} else {120})')
    edit(base, 'self.control.cancel();', 'if !crate::file2811_control("json-cancel") {self.control.cancel();}', 2)
    edit(base, '    let Some(snapshot) = &state.current else {',
         '    if crate::file2811_control("json-badge") {return div().into_any_element();}\n    let Some(snapshot) = &state.current else {')
    edit(base, 'Kind::Bottom => bottom(reader, state, cx),',
         'Kind::Bottom => if crate::file2811_control("bottom-line") {div().into_any_element()} else {bottom(&reader,state,cx)},')
    edit(base, 'if editing.conflict_detected {',
         'if editing.conflict_detected && !crate::file2811_control("conflict-enabled") {')
    edit(base, '} else if editing.save_failed {',
         '} else if editing.save_failed && !crate::file2811_control("failed-save-enabled") {')
    edit(base, '} else if editing.operations.busy {',
         '} else if editing.operations.busy && !crate::file2811_control("busy-enabled") {')
    edit(base, 'menu = menu.item(item);',
         'if !(action==Action::Validate && crate::file2811_control("dead-validate-menu")) {menu=menu.item(item);}')
    edit(base, '.debug_selector(|| "file-repair-sheet".into())\n        .absolute()',
         '.debug_selector(|| "file-repair-sheet".into())\n        .map(|s| if crate::file2811_control("flowing-sheet") {s.relative()} else {s.absolute()})')
    edit(base, 'e.operations.preview = None;\n                            e.operations.notice = None;',
         '''e.operations.preview = None;
                            e.operations.notice = None;
                            if crate::file2811_control("cancel-mutates") {e.input.update(cx,|i,cx|i.replace_source_range(0..0," ",w,cx));}''')
    edit(base, 'r.apply_code_repair(w, cx)',
         '{r.apply_code_repair(w,cx); if crate::file2811_control("apply-saves") {r.save_source(cx);}}')
    edit(base, 'self.toggle_source(window, cx);',
         'if !crate::file2811_control("read-repair") {self.toggle_source(window,cx);}')
    edit('reader_editor.rs', 'code.restore_presentation(saved, window, cx)',
         'if !crate::file2811_control("lost-presentation") {code.restore_presentation(saved, window, cx)}')
    edit('reader_code_file/presentation.rs', 'self.show_page(saved.page, window, cx);',
         'self.show_page(if crate::file2811_control("lost-page") {0} else {saved.page}, window, cx);')
    edit('main.rs', 'for context in ["ReaderFileEdit", "ReaderFileEdit > Input"] {',
         'for context in if crate::file2811_control("dead-shortcuts") {["Unused", "Unused > Input"]} else {["ReaderFileEdit", "ReaderFileEdit > Input"]} {')
    edit(base, 'reader_toast::transient(toast, window, cx);',
         'if !crate::file2811_control("lost-toast") {reader_toast::transient(toast,window,cx);}')
    edit(base, '    if text == "Valid strict JSON" {',
         '    if crate::file2811_control("bottom-wording") {return text.into();}\n    if text == "Valid strict JSON" {')
    edit(base, 'if language_bounds.get().is_none() {',
         'if language_bounds.get().is_none() && !crate::file2811_control("language-fallback") {')
    edit('reader_editor/code_operations.rs', 'if !editing.markdown {\n                editing.operations.preview = None;\n                self.publish_file_operation_notice(window, cx);',
         'if !editing.markdown && !crate::file2811_control("stale-conflict-sheet") {\n                editing.operations.preview = None;\n                self.publish_file_operation_notice(window, cx);')
    edit('reader_editor/code_operations.rs', 'usize::from(input.text().bytes().take(3).eq("\\u{feff}".bytes())) * 3',
         '(if crate::file2811_control("bom-body") {0} else {usize::from(input.text().bytes().take(3).eq("\\u{feff}".bytes())) * 3})')
    (OUT / 'mutations.diff').write_text(''.join(patch))
    if '--check-mutations' in sys.argv:
        print('FILE2811_MUTATION_ANCHORS_VALID', len(CONTROLS))
    else:
        build = run('compile-controls', BASE + ['--no-run', '--message-format=json-render-diagnostics'])
        assert build.returncode == 0, 'compiled controls must compile before any negative can count'
        artifacts = [json.loads(s) for s in build.stdout.splitlines() if s.startswith('{')]
        binary = next(x['executable'] for x in artifacts if x.get('reason') == 'compiler-artifact'
                      and x.get('executable') and x.get('profile', {}).get('test') and x['target']['name'] == 'okilum')
        positive(binary, 'platform::shaped_line_cache::tests::edit2803_native_geometry_', expected=3)
        positive(binary, 'brand::tests::every_local_functional_icon_is_embedded')
        positive(binary, 'file2811_')
        for control, test in CONTROLS:
            result = run('negative-' + control, [binary, test, '--nocapture', '--test-threads=1'], control)
            assert result.returncode != 0 and 'running 1 test' in result.stdout, (control, 'did not execute exactly one failing control')
            assert 'test result: FAILED' in result.stdout and 'panicked at' in result.stdout and test in result.stdout, control
            if control == 'geometry-partition':
                assert 'same-font native batching preserves exact geometry' in result.stdout, result.stdout
            print('FILE2811_COMPILED_NEGATIVE_FIRED', control, flush=True)
finally:
    for path, data in original.items():
        path.write_bytes(data)
assert all(p.read_bytes() == data for p, data in original.items())
if '--check-mutations' not in sys.argv:
    restored = run('restored-production', BASE + ['file2811_', '--', '--nocapture', '--test-threads=1'])
    assert restored.returncode == 0 and re.search(r'test result: ok\. [1-9]\d* passed', restored.stdout)
    restored_geometry = run('restored-native-geometry', BASE + [
        'platform::shaped_line_cache::tests::edit2803_native_geometry_', '--', '--nocapture', '--test-threads=1'])
    assert restored_geometry.returncode == 0 and re.search(
        r'test result: ok\. 3 passed; 0 failed;', restored_geometry.stdout)
    print('FILE2811_RESTORED_PRODUCTION_PASS', flush=True)
