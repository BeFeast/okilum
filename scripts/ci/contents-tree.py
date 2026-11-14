#!/usr/bin/env python3
"""Compile #2814 Contents negatives once; each must fail its named test."""
import difflib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

# Test output carries non-ASCII (→, «»); a Windows console codepage cannot
# print it, which failed the run after every control had passed.
sys.stdout.reconfigure(encoding='utf-8', errors='replace')
ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / 'target/contents-tree-controls'
OUT.mkdir(parents=True, exist_ok=True)
BASE = ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum',
        '--features', 'data-view-native-harness']
if '--release' in sys.argv:
    BASE.append('--release')
if '--linux-sprint' in sys.argv:
    BASE[BASE.index('data-view-native-harness')] = 'linux-sprint,data-view-native-harness'
if '--shell-no-opt' in sys.argv:
    BASE.extend(['--config', 'profile.release.package.okilum-shell.opt-level=0'])
ENV = {k:v for k,v in os.environ.items() if k != 'OKILUM_CONTENTS2814_CONTROL'}
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
    if control: env['OKILUM_CONTENTS2814_CONTROL'] = control
    p = subprocess.run(args, cwd=ROOT, env=env, text=True, encoding='utf-8', stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (OUT / (label + '.log')).write_text(p.stdout, encoding='utf-8')
    print(label, p.returncode, p.stdout[-3500:], flush=True)
    return p

CONTROLS = [
 ('open-all-depths', 'contents2814_two_levels_open_counts_short_values_and_plain_ordinals'),
 ('bracket-ordinals', 'contents2814_two_levels_open_counts_short_values_and_plain_ordinals'),
 ('no-groups', 'contents2814_groups_of_100_and_10000_with_a_bounded_click'),
 ('index-keyed', 'contents2814_expansion_is_keyed_by_path_and_survives_a_rebuild'),
 ('reading-immediate', 'contents2814_reading_line_waits_300_ms'),
 ('click-switches-view', 'contents2814_click_goes_to_the_node_in_every_view_without_switching'),
 ('forget-on-close', 'contents2814_open_rows_survive_view_switch_and_panel_close'),
 ('table-no-columns', 'contents2814_json_rows_states_and_table_columns'),
 ('labels-walk-children', 'contents2814_group_labels_cost_no_child_walk'),
 ('group-children-open', 'contents2814_children_of_an_opened_group_start_closed'),
 ('shared-duplicate-key', 'contents2814_repeated_keys_open_and_close_independently'),
 ('swap-doc-early', 'contents2814_rows_and_their_document_change_together'),
 ('timer-survives-switch', 'contents2814_reading_timer_restarts_for_each_file'),
 ('click-by-index', 'contents2814_a_click_on_old_rows_reaches_the_same_node_in_a_newer_document'),
 ('linked-from-everywhere', 'linked2885_data_files_hide_linked_from_and_notes_keep_it'),
 ('hide-linked-backlinks', 'linked2885_a_linked_data_file_keeps_linked_from'),
 ('json-only-contents', 'contents2814_yaml_toml_xml_rows_states_and_where_you_are'),
 ('no-current-mark', 'contents2814_yaml_toml_xml_rows_states_and_where_you_are'),
 ('indent-current', 'contents2814_a_caret_move_opens_the_path_and_scrolling_never_opens'),
 ('remap-every-sample', 'contents2814_a_newer_parse_costs_viewport_samples_no_child_walk'),
 ('groups-unmarked', 'contents2814_where_you_are_inside_a_closed_group_marks_the_group'),
 ('caret-never-leads', 'contents2814_a_caret_move_opens_the_path_and_scrolling_never_opens'),
 ('scroll-opens', 'contents2814_a_caret_move_opens_the_path_and_scrolling_never_opens'),
 ('no-path-open', 'contents2814_a_caret_move_opens_the_path_and_scrolling_never_opens'),
 ('scroll-wins', 'contents2814_a_caret_move_opens_the_path_and_scrolling_never_opens'),
 ('tree-selection-ignored', 'contents2814_find_hits_and_tree_selection_become_where_you_are'),
 ('scroll-before-landed', 'contents2814_a_newer_parse_costs_viewport_samples_no_child_walk'),
 ('caret-sticks', 'contents2814_scrolling_right_after_a_caret_move_follows_the_top'),
 ('jump-gen-ignored', 'contents2814_a_repeated_jump_after_scrolling_away_marks_the_node_again'),
 ('local-caret', 'contents2814_find_hits_at_the_same_local_offset_on_adjacent_pages'),
 ('reveal-on-ui', 'contents2814_a_reveal_through_a_huge_container_costs_the_ui_thread_no_child_walk'),
 ('page-jump-is-caret', 'contents2814_a_page_jump_is_scrolling_and_a_click_on_another_page_is_a_caret_move'),
 ('page-caret-ignored', 'contents2814_a_page_jump_is_scrolling_and_a_click_on_another_page_is_a_caret_move'),
 ('edit-reads-viewer', 'contents2814_edit_follows_each_parse_keeps_open_rows_and_the_last_valid_text'),
 ('edit-no-stale', 'contents2814_edit_follows_each_parse_keeps_open_rows_and_the_last_valid_text'),
 ('edit-unmapped-jump', 'contents2814_edit_follows_each_parse_keeps_open_rows_and_the_last_valid_text'),
 ('edit-forget-open', 'contents2814_edit_follows_each_parse_keeps_open_rows_and_the_last_valid_text'),
 ('yaml-no-verdict', 'contents2814_edit_yaml_is_stale_while_invalid_and_toml_keeps_tolerant_rows'),
 ('map-unchanged', 'contents2814_offsets_map_through_line_edits'),
 ('edit-no-whereabouts', 'contents2814_edit_follows_each_parse_keeps_open_rows_and_the_last_valid_text'),
 ('reload-change-only', 'contents2814_edit_reload_with_other_content_refreshes_contents_and_clears_the_warning'),
 ('drop-on-ui', 'contents2814_edit_never_releases_a_tree_on_the_ui_thread'),
 ('columns-no-mark', 'contents2814_csv_and_tsv_list_their_columns_and_follow_the_selected_cell'),
 ('column-jump-row', 'contents2814_csv_and_tsv_list_their_columns_and_follow_the_selected_cell'),
 ('column-no-scroll', 'contents2814_a_wide_table_without_a_header_scrolls_sideways_to_the_column'),
 ('columns-ignore-header', 'contents2814_a_wide_table_without_a_header_scrolls_sideways_to_the_column'),
 ('columns-stay', 'contents2814_a_json_array_lists_columns_only_in_table'),
 ('edit-keeps-columns', 'contents2814_entering_edit_drops_the_column_list'),
 ('column-no-rows-select', 'contents2814_a_column_click_after_a_find_with_no_rows_selects_nothing'),
 ('column-tooltip-cut', 'contents2814_long_headers_sharing_a_prefix_keep_full_names_in_the_tooltip'),
]
TESTS = ['contents2814_', 'linked2885_']
CONTROL_FN = """#[cfg(test)]
fn contents2814_control(name: &str) -> bool {
    std::env::var("OKILUM_CONTENTS2814_CONTROL").is_ok_and(|v| v == name)
}
fn main() {"""
try:
    edit('main.rs', 'fn main() {', CONTROL_FN)
    tree = 'reader_file_outline/tree.rs'
    edit(tree, 'children > 0 && children <= OPEN_CHILDREN && doc.structure[index].depth <= OPEN_DEPTH',
         'children > 0 && (crate::contents2814_control("open-all-depths") || children <= OPEN_CHILDREN && doc.structure[index].depth <= OPEN_DEPTH)')
    edit(tree, '(None, Some(n)) => n.to_string(),', '(None, Some(n)) => if crate::contents2814_control("bracket-ordinals") { format!("[{n}]") } else { n.to_string() },')
    edit(tree, '} else if count > GROUP {', '} else if count > GROUP && !crate::contents2814_control("no-groups") {')
    edit(tree, '            let segment = match (doc.key_text(child), doc.array_ordinal(child)) {\n', '            let segment = match (doc.key_text(child), doc.array_ordinal(child)) {\n                _ if crate::contents2814_control("index-keyed") => format!("@{child}"),\n')
    edit(tree, '                    let segment = if *n == 0 {', '                    let segment = if *n == 0 || crate::contents2814_control("shared-duplicate-key") {')
    edit(tree, 'default_open: !grouped && default_open(doc, index),', 'default_open: (!grouped || crate::contents2814_control("group-children-open")) && default_open(doc, index),')
    edit(tree, '    if doc.key_text(first_index).is_some() {', '    let first_index = if crate::contents2814_control("labels-walk-children") { doc.children(doc.parent(first_index).unwrap()).nth(first).unwrap() } else { first_index };\n    if doc.key_text(first_index).is_some() {')
    view = 'reader_file_outline.rs'
    edit(view, '    fn snapshot_current(&mut self, cx: &mut Context<Self>) -> Option<usize> {\n', '    fn snapshot_current(&mut self, cx: &mut Context<Self>) -> Option<usize> {\n        if crate::contents2814_control("no-current-mark") {\n            return None;\n        }\n')
    edit(view, '            if !Arc::ptr_eq(&raw, &doc) {\n                return self.snapshot.current;\n            }\n', '            if !Arc::ptr_eq(&raw, &doc) {\n                if crate::contents2814_control("remap-every-sample") {\n                    let node = code.read(cx).contents_whereabouts(cx).and_then(|w| w.top_node)?;\n                    raw.node_in(node, &doc)?;\n                }\n                return self.snapshot.current;\n            }\n')
    edit(view, '        let groups = self.snapshot.groups.get(&parent)?;', '        if crate::contents2814_control("groups-unmarked") {\n            return None;\n        }\n        let groups = self.snapshot.groups.get(&parent)?;')
    edit(view, '                self.snapshot.follow_caret = true;\n', '                self.snapshot.follow_caret = !crate::contents2814_control("caret-never-leads");\n')
    edit(view, '                self.snapshot.reveal = None;\n            }\n            _ => {}', '                self.snapshot.reveal = if crate::contents2814_control("scroll-opens") { now.top_node.map(|node| (node, false)) } else { None };\n            }\n            _ => {}')
    edit(view, '                        tree::open_path(&doc, node, &mut expansion);', '                        if !crate::contents2814_control("no-path-open") {\n                            tree::open_path(&doc, node, &mut expansion);\n                        }')
    edit(view, '                    && now.caret_node.is_some() =>\n', '                    && now.caret_node.is_some()\n                    && (before.top == now.top || !crate::contents2814_control("scroll-wins")) =>\n')
    edit(view, '                        && now.caret == Some((now.page, 0))\n                        && before.jumps == now.jumps) =>', '                        && (crate::contents2814_control("page-caret-ignored") || now.caret == Some((now.page, 0)) && !crate::contents2814_control("page-jump-is-caret"))\n                        && (before.jumps == now.jumps || crate::contents2814_control("page-caret-ignored"))) =>')
    edit(view, 'Some(before) if before.top != now.top && before.settled && now.settled => {', 'Some(before) if before.top != now.top && (crate::contents2814_control("scroll-before-landed") || before.settled && now.settled) && (!crate::contents2814_control("caret-sticks") || !self.snapshot.follow_caret) => {')
    edit(view, '                self.rebuild_rows_revealing(Some(node), cx);', '                if crate::contents2814_control("reveal-on-ui") {\n                    tree::open_path(&doc, node, self.expansion());\n                }\n                self.rebuild_rows_revealing(Some(node), cx);')
    edit(view, '            .zip(Self::edit_contents_language(reader))', '            .zip(Self::edit_contents_language(reader).filter(|_| !crate::contents2814_control("edit-reads-viewer")))')
    edit(view, '        let (line, message) = self.snapshot.edit.as_ref()?.stale.as_ref()?;', '        if crate::contents2814_control("edit-no-stale") {\n            return None;\n        }\n        let (line, message) = self.snapshot.edit.as_ref()?.stale.as_ref()?;')
    ed = 'reader_file_outline/edit.rs'
    edit(ed, '        self.snapshot.columns = None;\n        self.snapshot.active = true;', '        if !crate::contents2814_control("edit-keeps-columns") {\n            self.snapshot.columns = None;\n        }\n        self.snapshot.active = true;')
    edit(ed, '        let edit = self.snapshot.edit.as_ref()?;\n        let (built, stamp) = edit.built.as_ref()?;', '        let edit = self.snapshot.edit.as_ref()?;\n        if crate::contents2814_control("edit-no-whereabouts") {\n            return None;\n        }\n        let (built, stamp) = edit.built.as_ref()?;')
    edit(ed, '        if current {', '        if current || crate::contents2814_control("edit-unmapped-jump") {')
    edit(ed, '        retire(edit.built.replace((doc.clone(), stamp)), cx);', '        retire(edit.built.replace((doc.clone(), stamp)), cx);\n        if crate::contents2814_control("edit-forget-open") {\n            *self.expansion() = Default::default();\n        }')
    edit(ed, 'cx.subscribe(&input, |this, _, _: &SourceMutation, cx| {', 'cx.subscribe(&input, |this, _, event: &SourceMutation, cx| {\n                if crate::contents2814_control("reload-change-only") && matches!(event.reason, gpui_component::input::projection::SourceMutationReason::Reset) {\n                    return;\n                }')
    edit(view, "fn retire<T: Send + 'static>(value: T, cx: &App) {\n", "fn retire<T: Send + 'static>(value: T, cx: &App) {\n    if crate::contents2814_control(\"drop-on-ui\") {\n        drop(value);\n        return;\n    }\n")
    edit(ed, '                if let Some(error) = &doc.invalid {', '                if let Some(error) = doc.invalid.as_ref().filter(|_| !crate::contents2814_control("yaml-no-verdict")) {')
    edit(ed, '    if old == new {', '    if old == new || crate::contents2814_control("map-unchanged") {')
    edit(view, 'cx.background_executor().timer(READING_DELAY).await;', 'if !crate::contents2814_control("reading-immediate") {\n                                cx.background_executor().timer(READING_DELAY).await;\n                            }')
    edit(view, 'let expansions = std::mem::take(&mut self.snapshot.expansions);', 'let expansions = if crate::contents2814_control("forget-on-close") { Default::default() } else { std::mem::take(&mut self.snapshot.expansions) };')
    edit(view, '                    retire(self.snapshot.pending.replace(doc), cx);', '                    if crate::contents2814_control("swap-doc-early") {\n                        self.snapshot.doc = Some(doc.clone());\n                    }\n                    retire(self.snapshot.pending.replace(doc), cx);')
    edit(view, '            self.snapshot.reading = None;\n            self.snapshot.state = SnapshotState::Absent;\n            self.list.reset(0);', '            if !crate::contents2814_control("timer-survives-switch") {\n                self.snapshot.reading = None;\n                self.snapshot.state = SnapshotState::Absent;\n            }\n            self.list.reset(0);')
    edit('reader_right_panel.rs', 'let service = service_file(reader) && reader.backlinks.is_empty();', 'let service = service_file(reader) && (reader.backlinks.is_empty() || crate::contents2814_control("hide-linked-backlinks"));')
    edit('reader_right_panel.rs', '    reader.file_preview.as_ref().is_some_and(|preview| {', '    !crate::contents2814_control("linked-from-everywhere") && reader.file_preview.as_ref().is_some_and(|preview| {')
    code = 'reader_code_file/contents.rs'
    edit(code, '                    caret: self.structure.selected.map(|row| (0, row)),', '                    caret: self.structure.selected.map(|row| (0, row)).filter(|_| !crate::contents2814_control("tree-selection-ignored")),')
    edit(code, '                    caret: Some((self.page, cursor)),', '                    caret: Some((if crate::contents2814_control("local-caret") { 0 } else { self.page }, cursor)),')
    edit(code, '        self.contents_jumps += 1;', '        if !crate::contents2814_control("jump-gen-ignored") {\n            self.contents_jumps += 1;\n        }')
    edit(code, '            || matches!(language(&self.rel), Some("yaml" | "toml" | "xml"))', '            || (!crate::contents2814_control("json-only-contents") && matches!(language(&self.rel), Some("yaml" | "toml" | "xml")))')
    edit(code, ".take_while(|b| *b == b' ' || *b == b'\\t')", ".take_while(|b| !crate::contents2814_control(\"indent-current\") && (*b == b' ' || *b == b'\\t'))")
    edit(code, '        let index = if Arc::ptr_eq(&raw, rows) {', '        let index = if Arc::ptr_eq(&raw, rows) || crate::contents2814_control("click-by-index") {')
    edit(code, '            View::Tree => {\n                self.structure.reveal_node(index, &active);', '            View::Tree => {\n                if crate::contents2814_control("click-switches-view") {\n                    self.apply_view(View::Raw, window, cx);\n                }\n                self.structure.reveal_node(index, &active);')
    edit(code, '        self.table.clone().filter(|_| self.view == View::Table)', '        self.table.clone().filter(|_| self.view == View::Table && !crate::contents2814_control("table-no-columns"))')
    edit(view, '            (None, Some(_)) => {\n                self.snapshot.columns = None;', '            (None, Some(_)) => {\n                if crate::contents2814_control("columns-stay") {\n                    return;\n                }\n                self.snapshot.columns = None;')
    dl = 'reader_delimited/chrome.rs'
    edit(dl, '        self.end.map(|cell| cell.column)\n    }', '        self.end.map(|cell| cell.column).filter(|_| !crate::contents2814_control("columns-no-mark"))\n    }')
    edit(dl, '            .unwrap_or(first);', '            .unwrap_or(first);\n        let row = if crate::contents2814_control("column-jump-row") { first } else { row };')
    edit(dl, '        self.reveal_column_offset(column);\n        self.focus', '        if !crate::contents2814_control("column-no-scroll") {\n            self.reveal_column_offset(column);\n        }\n        self.focus')
    edit(dl, '                let cell = if self.header {', '                let cell = if self.header || crate::contents2814_control("columns-ignore-header") {')
    edit(dl, '                    (visible(cell), cell.to_owned())', '                    (visible(cell), if crate::contents2814_control("column-tooltip-cut") { visible(cell) } else { cell.to_owned() })')
    edit(dl, '        if self.order.is_empty() {', '        if self.order.is_empty() && !crate::contents2814_control("column-no-rows-select") {')
    (OUT / 'mutations.diff').write_text('\n'.join(patches))
    if '--audit' in sys.argv:
        print('CONTENTS2814_MUTATION_ANCHORS_PASS', flush=True)
        raise SystemExit(0)
    result = run('compile', BASE + ['--no-run', '--message-format=json-render-diagnostics'])
    assert result.returncode == 0, 'compiled controls must build'
    rows = [json.loads(s) for s in result.stdout.splitlines() if s.startswith('{')]
    binary = next(x['executable'] for x in rows if x.get('reason') == 'compiler-artifact' and x.get('executable')
                  and x.get('profile', {}).get('test') and x['target']['name'] == 'okilum')
    for prefix in TESTS:
        result = run('positive-' + prefix, [binary, prefix, '--test-threads=1'])
        assert result.returncode == 0 and re.search(r'test result: ok\. [1-9]\d* passed;', result.stdout), prefix
    for control, test in CONTROLS:
        result = run(control, [binary, test, '--test-threads=1'], control)
        assert (result.returncode != 0 and 'running 1 test' in result.stdout
                and 'test result: FAILED' in result.stdout and 'panicked at' in result.stdout), control
        print('CONTENTS2814_COMPILED_NEGATIVE_FIRED', control, flush=True)
finally:
    for p, data in original.items():
        p.write_bytes(data)
assert all(p.read_bytes() == data for p, data in original.items())
print('CONTENTS2814_RESTORED', flush=True)
