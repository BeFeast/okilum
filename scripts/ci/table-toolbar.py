#!/usr/bin/env python3
"""Compile table-toolbar negatives once and exercise real projection/UI paths."""
import difflib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / 'target/table-toolbar-controls'
OUT.mkdir(parents=True, exist_ok=True)
BASE = ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum',
        '--features', 'data-view-native-harness']
if '--release' in sys.argv:
    BASE.append('--release')
if '--linux-sprint' in sys.argv:
    BASE[BASE.index('data-view-native-harness')] = 'linux-sprint,data-view-native-harness'
if '--shell-no-opt' in sys.argv:
    BASE.extend(['--config', 'profile.release.package.okilum-shell.opt-level=0'])
ENV = {k:v for k,v in os.environ.items() if k != 'OKILUM_TABLE2812_CONTROL'}
original = {}
patches = []

def edit(name, old, new, count=1):
    p = ROOT / ('crates/okilum-shell/src/' + name)
    # Bytes in and out: no platform codepage and no newline translation.
    s = p.read_bytes().decode('utf-8')
    assert s.count(old) == count, (name, old, s.count(old))
    original.setdefault(p, p.read_bytes())
    result = s.replace(old, new)
    p.write_bytes(result.encode('utf-8'))
    patches.append(''.join(difflib.unified_diff(s.splitlines(True), result.splitlines(True), fromfile=name, tofile=name)))

def run(label, args, control=None):
    env = dict(ENV)
    if control: env['OKILUM_TABLE2812_CONTROL'] = control
    p = subprocess.run(args, cwd=ROOT, env=env, text=True, encoding='utf-8', stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (OUT / (label + '.log')).write_text(p.stdout, encoding='utf-8')
    print(label, p.returncode, p.stdout[-3500:], flush=True)
    return p

CONTROLS = [
 ('eager-raw', 'csv_is_lazy_table_then_readonly_raw_and_real_edit'),
 ('strand-focus', 'csv_is_lazy_table_then_readonly_raw_and_real_edit'),
 ('first-cell-only', 'matching_cells_case_steps_and_escape_keep_selection'),
 ('previous-is-next', 'matching_cells_case_steps_and_escape_keep_selection'),
 ('ignore-case', 'matching_cells_case_steps_and_escape_keep_selection'),
 ('lose-selection', 'matching_cells_case_steps_and_escape_keep_selection'),
 ('no-horizontal-reveal', 'matching_cells_case_steps_and_escape_keep_selection'),
 ('reset-always-enabled', 'sort_header_toggle_status_and_copy_toast'),
 ('dead-reset', 'sort_header_toggle_status_and_copy_toast'),
 ('dead-header', 'sort_header_toggle_status_and_copy_toast'),
 ('ungrouped-count', 'sort_header_toggle_status_and_copy_toast'),
 ('no-copy-toast', 'sort_header_toggle_status_and_copy_toast'),
 ('dead-load-more', 'sort_header_toggle_status_and_copy_toast'),
 ('dead-badge', 'malformed_badge_reveals_filtered_row_and_failed_table_opens_raw'),
 ('no-raw-fallback', 'malformed_badge_reveals_filtered_row_and_failed_table_opens_raw'),
 ('json-header-toggle', 'json_has_four_views_and_no_header_toggle'),
 ('query-in-header-key', 'find_debounces_and_notifies_no_header_or_side_panels'),
 ('no-debounce', 'find_debounces_and_notifies_no_header_or_side_panels'),
 ('no-duckdb-toast', 'duckdb_running_menu_and_missing_cli_toast'),
 ('duckdb-not-checked', 'duckdb_running_menu_and_missing_cli_toast'),
 ('no-view-memory', 'csv_tsv_memory_is_separate_from_json_array'),
 ('no-raw-cap', 'raw_cap_does_not_eagerly_read_a_large_csv'),
 ('drop-lock', 'read_only_lock_stays_by_the_name_in_table'),
 ('raw-before-admission', 'remembered_raw_yields_to_table_when_raw_is_over_its_budget'),
 ('csv-edit-cap', 'five_mib_csv_still_opens_the_source_editor'),
 ('scan-survives-hide', 'find_scan_stops_when_table_is_hidden_or_closed'),
 ('scan-survives-close', 'find_scan_stops_when_table_is_hidden_or_closed'),
 ('edit-keeps-scanning', 'edit_stops_table_find_and_read_runs_it_once'),
 ('hidden-refilter', 'hidden_load_more_waits_and_runs_the_query_once_on_return'),
 ('hidden-table-badge', 'malformed_badge_reveals_filtered_row_and_failed_table_opens_raw'),
 ('window-sized-compact', 'narrow_header_and_measured_compact_pane_keep_controls_inside'),
]
try:
    edit('main.rs', 'fn main() {', '''#[cfg(test)]
fn table2812_control(name: &str) -> bool {
    std::env::var("OKILUM_TABLE2812_CONTROL").is_ok_and(|v|v==name)
}
fn main() {''')
    edit('reader_code_file.rs', 'if initial == View::Raw {', 'if initial == View::Raw || crate::table2812_control("eager-raw") {')
    edit('reader_code_file/view.rs', 'if focused_table && self.view != View::Table {', 'if focused_table && self.view != View::Table && !crate::table2812_control("strand-focus") {')
    edit('reader_code_file/toolbar.rs', 'None => table_chrome.state = Some(own),', 'None if crate::table2812_control("drop-lock") => {}\n                        None => table_chrome.state = Some(own),')
    edit('reader_code_file/toolbar.rs', 'None => chrome.state = Some(warning),', 'None if crate::table2812_control("hidden-table-badge") => {}\n                None => chrome.state = Some(warning),')
    edit('reader_code_file/loading.rs', '        if reader_delimited::eligible(&self.rel)\n            && self.view == View::Raw\n            && self.table.is_some()', '        if !crate::table2812_control("raw-before-admission")\n            && reader_delimited::eligible(&self.rel)\n            && self.view == View::Raw\n            && self.table.is_some()')
    edit('reader_code_file.rs', '.is_some_and(|bytes| reader_delimited::eligible(&self.rel) || bytes <= EDIT_BYTES)', '.is_some_and(|bytes| (reader_delimited::eligible(&self.rel) && !crate::table2812_control("csv-edit-cap")) || bytes <= EDIT_BYTES)')
    edit('reader_delimited.rs', '        self.projection_cancel\n            .store(true, std::sync::atomic::Ordering::Relaxed);', '        if !crate::table2812_control("scan-survives-close") {\n            self.projection_cancel\n                .store(true, std::sync::atomic::Ordering::Relaxed);\n        }')
    edit('reader_code_file/view.rs', '        if let Some(table) = &self.table {\n            table.update(cx, |table, _| table.suspend_work());\n        }', '        if let Some(table) = self.table.as_ref().filter(|_| !crate::table2812_control("edit-keeps-scanning")) {\n            table.update(cx, |table, _| table.suspend_work());\n        }')
    edit('reader_delimited/find.rs', '        if self.suspended {\n            self.resume_pending = true;', '        if self.suspended && !crate::table2812_control("hidden-refilter") {\n            self.resume_pending = true;')
    find = 'reader_delimited/find.rs'
    edit(find, '.matches\n                    .into_iter()', '.matches\n                    .into_iter().take(if crate::table2812_control("first-cell-only") {1} else {usize::MAX})')
    edit(find, 'i64::from(direction)', 'i64::from(if crate::table2812_control("previous-is-next") {direction.abs()} else {direction})')
    edit(find, 'usize::from(self.header),\n            self.case_sensitive,', 'usize::from(self.header),\n            self.case_sensitive && !crate::table2812_control("ignore-case"),')
    edit(find, 'if let Some((a, b)) = old_selection {', 'if let Some((a, b)) = old_selection.filter(|_|!crate::table2812_control("lose-selection")) {')
    edit(find, 'self.horizontal.set_offset(point(px(-next.max(0.)), px(0.)));', 'if !crate::table2812_control("no-horizontal-reveal") {self.horizontal.set_offset(point(px(-next.max(0.)), px(0.)));}')
    edit(find, 'if self.projection_task.is_some() && self.updating {', 'if !crate::table2812_control("scan-survives-hide") && self.projection_task.is_some() && self.updating {')
    edit(find, 'Duration::from_millis(60)', 'Duration::from_millis(if crate::table2812_control("no-debounce") {0} else {60})')
    chrome = 'reader_delimited/chrome.rs'
    edit(chrome, 'duck.toggled = self.external_pending;', 'duck.toggled = self.external_pending && !crate::table2812_control("duckdb-not-checked");')
    edit(chrome, 'reset.enabled = self.sort.is_some();', 'reset.enabled = self.sort.is_some() || crate::table2812_control("reset-always-enabled");')
    edit(chrome, 'if self.sort.take().is_some() {', 'if !crate::table2812_control("dead-reset") && self.sort.take().is_some() {')
    edit(chrome, 'self.header = !self.header;', 'if !crate::table2812_control("dead-header") {self.header = !self.header;}')
    edit(chrome, 'fn grouped(value: usize) -> String {', 'fn grouped(value: usize) -> String {\n    if crate::table2812_control("ungrouped-count") {return value.to_string();}')
    edit(chrome, '            reader_toast::transient(', '            if crate::table2812_control("no-copy-toast") {return;}\n            reader_toast::transient(')
    edit(chrome, 's.load(true, cx)', 'if !crate::table2812_control("dead-load-more") {s.load(true, cx)}')
    edit(chrome, 'pub(super) fn go_to_bad_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {', 'pub(super) fn go_to_bad_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {\n        if crate::table2812_control("dead-badge") {return;}')
    edit('reader_code_file/loading.rs', 'this.apply_view(View::Raw, window, cx);', 'if !crate::table2812_control("no-raw-fallback") {this.apply_view(View::Raw, window, cx);}')
    edit(chrome, 'if !self.json() {', 'if !self.json() || crate::table2812_control("json-header-toggle") {')
    edit('reader_viewer_chrome.rs', 'reader.selected_title(),', 'reader.selected_title()+&if crate::table2812_control("query-in-header-key") {reader.viewer_find_target(cx).map(|f|f.input.read(cx).value().to_string()).unwrap_or_default()} else {String::new()},')
    edit('reader_toast.rs', 'pub(super) fn duckdb_error(message: String, window: &mut Window, cx: &mut App) {', 'pub(super) fn duckdb_error(message: String, window: &mut Window, cx: &mut App) {\n    if crate::table2812_control("no-duckdb-toast") {return;}')
    edit('reader_code_file/view.rs', 'reader_ui_state::set_code_view(kind, view, cx);', 'if !crate::table2812_control("no-view-memory") {reader_ui_state::set_code_view(kind, view, cx);}')
    edit('reader_code_file/view.rs', 'self.source_bytes.is_some_and(|n| n as u64 > MAX_BYTES)', 'self.source_bytes.is_some_and(|n| n as u64 > MAX_BYTES) && !crate::table2812_control("no-raw-cap")')
    edit('reader_code_file.rs', '&chrome,\n                        bounds.size.width.as_f32(),', '&chrome,\n                        if crate::table2812_control("window-sized-compact") {window.viewport_size().width.as_f32()} else {bounds.size.width.as_f32()},')
    (OUT / 'mutations.diff').write_text('\n'.join(patches))
    if '--audit' in sys.argv:
        print('TABLE2812_MUTATION_ANCHORS_PASS', flush=True)
        raise SystemExit(0)
    result = run('compile', BASE + ['--no-run', '--message-format=json-render-diagnostics'])
    assert result.returncode == 0, 'compiled controls must build'
    rows = [json.loads(s) for s in result.stdout.splitlines() if s.startswith('{')]
    binary = next(x['executable'] for x in rows if x.get('reason')=='compiler-artifact' and x.get('executable') and x.get('profile',{}).get('test') and x['target']['name']=='okilum')
    result = run('positive', [binary, 'table2812_', '--nocapture', '--test-threads=1'])
    assert result.returncode == 0 and re.search(r'test result: ok\. (1[7-9]|[2-9]\d) passed; 0 failed', result.stdout)
    for control, suffix in CONTROLS:
        result = run(control, [binary,'table2812_'+suffix,'--nocapture','--test-threads=1'],control)
        assert result.returncode != 0 and 'running 1 test' in result.stdout and 'test result: FAILED' in result.stdout and 'panicked at' in result.stdout, control
        print('TABLE2812_COMPILED_NEGATIVE_FIRED', control, flush=True)
finally:
    for p,data in original.items():p.write_bytes(data)
assert all(p.read_bytes()==data for p,data in original.items())
result = run('restored',BASE + ['table2812_', '--', '--nocapture', '--test-threads=1'])
assert result.returncode == 0 and re.search(r'test result: ok\. (1[7-9]|[2-9]\d) passed; 0 failed', result.stdout)
print('TABLE2812_RESTORED_SUITE_PASS',flush=True)
