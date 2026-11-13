#!/usr/bin/env python3
"""Compile #2814 Contents negatives once; each must fail its named test."""
import difflib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

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
 ('table-contents', 'contents2814_json_rows_states_and_absent_table'),
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
 ('no-jump-settle', 'contents2814_a_newer_parse_costs_viewport_samples_no_child_walk'),
 ('page-jump-is-caret', 'contents2814_a_page_jump_is_scrolling_and_a_click_on_another_page_is_a_caret_move'),
 ('page-caret-ignored', 'contents2814_a_page_jump_is_scrolling_and_a_click_on_another_page_is_a_caret_move'),
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
    edit(view, '        if !Arc::ptr_eq(&raw, &doc) {\n            return self.snapshot.current;\n        }\n', '        if !Arc::ptr_eq(&raw, &doc) {\n            if crate::contents2814_control("remap-every-sample") {\n                let node = code.read(cx).contents_whereabouts(cx).and_then(|w| w.top_node)?;\n                raw.node_in(node, &doc)?;\n            }\n            return self.snapshot.current;\n        }\n')
    edit(view, '        let groups = self.snapshot.groups.get(&parent)?;', '        if crate::contents2814_control("groups-unmarked") {\n            return None;\n        }\n        let groups = self.snapshot.groups.get(&parent)?;')
    edit(view, '                self.snapshot.follow_caret = true;\n', '                self.snapshot.follow_caret = !crate::contents2814_control("caret-never-leads");\n')
    edit(view, '            {\n                self.snapshot.follow_caret = false;\n                self.snapshot.reveal = None;\n            }\n            _ => {}', '            {\n                self.snapshot.follow_caret = false;\n                self.snapshot.reveal = if crate::contents2814_control("scroll-opens") { now.top_node.map(|node| (node, false)) } else { None };\n            }\n            _ => {}')
    edit(view, 'const JUMP_SETTLE: Duration = Duration::from_millis(200);', 'const JUMP_SETTLE: Duration = Duration::from_millis(200);\nfn jump_settle() -> Duration { if crate::contents2814_control("no-jump-settle") { Duration::ZERO } else { JUMP_SETTLE } }')
    edit(view, '.saturating_duration_since(at) >= JUMP_SETTLE', '.saturating_duration_since(at) >= jump_settle()')
    edit(view, '                tree::open_path(&doc, node, self.expansion());', '                if !crate::contents2814_control("no-path-open") {\n                    tree::open_path(&doc, node, self.expansion());\n                }')
    edit(view, 'Some(before) if before.caret != now.caret && now.caret_node.is_some() => {', 'Some(before) if before.caret != now.caret && now.caret_node.is_some() && (before.top == now.top || !crate::contents2814_control("scroll-wins")) => {')
    edit(view, '|| (before.page != now.page && now.caret == Some(0))', '|| (before.page != now.page && (crate::contents2814_control("page-caret-ignored") || now.caret == Some(0) && !crate::contents2814_control("page-jump-is-caret")))')
    edit(view, 'cx.background_executor().timer(READING_DELAY).await;', 'if !crate::contents2814_control("reading-immediate") {\n                                cx.background_executor().timer(READING_DELAY).await;\n                            }')
    edit(view, 'let expansions = std::mem::take(&mut self.snapshot.expansions);', 'let expansions = if crate::contents2814_control("forget-on-close") { Default::default() } else { std::mem::take(&mut self.snapshot.expansions) };')
    edit(view, '                    self.snapshot.pending = Some(doc);', '                    if crate::contents2814_control("swap-doc-early") {\n                        self.snapshot.doc = Some(doc.clone());\n                    }\n                    self.snapshot.pending = Some(doc);')
    edit(view, '            self.snapshot.reading = None;\n            self.snapshot.state = SnapshotState::Absent;\n            self.list.reset(0);', '            if !crate::contents2814_control("timer-survives-switch") {\n                self.snapshot.reading = None;\n                self.snapshot.state = SnapshotState::Absent;\n            }\n            self.list.reset(0);')
    edit('reader_right_panel.rs', 'let service = service_file(reader) && reader.backlinks.is_empty();', 'let service = service_file(reader) && (reader.backlinks.is_empty() || crate::contents2814_control("hide-linked-backlinks"));')
    edit('reader_right_panel.rs', '    reader.file_preview.as_ref().is_some_and(|preview| {', '    !crate::contents2814_control("linked-from-everywhere") && reader.file_preview.as_ref().is_some_and(|preview| {')
    code = 'reader_code_file/contents.rs'
    edit(code, '                    caret: self.structure.selected,', '                    caret: self.structure.selected.filter(|_| !crate::contents2814_control("tree-selection-ignored")),')
    edit(code, '            || matches!(language(&self.rel), Some("yaml" | "toml" | "xml"))', '            || (!crate::contents2814_control("json-only-contents") && matches!(language(&self.rel), Some("yaml" | "toml" | "xml")))')
    edit(code, ".take_while(|b| *b == b' ' || *b == b'\\t')", ".take_while(|b| !crate::contents2814_control(\"indent-current\") && (*b == b' ' || *b == b'\\t'))")
    edit(code, '        let index = if Arc::ptr_eq(&raw, rows) {', '        let index = if Arc::ptr_eq(&raw, rows) || crate::contents2814_control("click-by-index") {')
    edit(code, '            View::Tree => {\n                self.structure.reveal_node(index, &active);', '            View::Tree => {\n                if crate::contents2814_control("click-switches-view") {\n                    self.apply_view(View::Raw, window, cx);\n                }\n                self.structure.reveal_node(index, &active);')
    edit(code, 'if !self.contents_format() || self.view == View::Table {', 'if !self.contents_format() || (self.view == View::Table && !crate::contents2814_control("table-contents")) {')
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
