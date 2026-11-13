#!/usr/bin/env python3
"""Compile #2813 structured-tree negatives once; each must fail its named test."""
import difflib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / 'target/structured-tree-controls'
OUT.mkdir(parents=True, exist_ok=True)
BASE = ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum',
        '--features', 'data-view-native-harness']
if '--release' in sys.argv:
    BASE.append('--release')
if '--linux-sprint' in sys.argv:
    BASE[BASE.index('data-view-native-harness')] = 'linux-sprint,data-view-native-harness'
if '--shell-no-opt' in sys.argv:
    BASE.extend(['--config', 'profile.release.package.okilum-shell.opt-level=0'])
ENV = {k:v for k,v in os.environ.items() if k != 'OKILUM_TREE2813_CONTROL'}
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
    if control: env['OKILUM_TREE2813_CONTROL'] = control
    p = subprocess.run(args, cwd=ROOT, env=env, text=True, encoding='utf-8', stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (OUT / (label + '.log')).write_text(p.stdout, encoding='utf-8')
    print(label, p.returncode, p.stdout[-3500:], flush=True)
    return p

CONTROLS = [
 ('name-before-id', 'xml2813_elements_take_id_then_name_and_attributes_are_not_rows'),
 ('comments-as-rows', 'xml2813_elements_take_id_then_name_and_attributes_are_not_rows'),
 ('root-folds', 'xml2813_root_stays_open_and_fold_all_keeps_it_open'),
 ('no-toml-merge', 'toml2813_tables_keys_and_merged_array_of_tables'),
 ('first-document-only', 'yaml2813_keys_sequences_anchors_and_documents'),
 ('grammar-verdict', 'verdicts2813_yaml_strict_toml_and_xml_syntax_only'),
 ('syntax-is-invalid', 'verdicts2813_yaml_strict_toml_and_xml_syntax_only'),
 ('toml-comment-value', 'toml2813_trailing_comments_never_replace_values'),
 ('yaml-comment-content', 'yaml2813_comments_after_anchor_tag_or_dash_keep_the_content'),
 ('no-subtable-merge', 'toml2813_sub_tables_between_array_blocks_stay_in_one_array_row'),
 ('root-toggle-closes', 'xml2813_root_chevron_and_left_cannot_close_the_root'),
 ('tree-json-only', 'data2813_tree_find_in_normal_size_yaml_and_xml_opens_parents_and_marks_the_row'),
]
TESTS = 'data2813_ xml2813_ toml2813_ yaml2813_ verdicts2813_ corpus2813_'.split()
CONTROL_FN = '''#[cfg(test)]
fn tree2813_control(name: &str) -> bool {
    std::env::var("OKILUM_TREE2813_CONTROL").is_ok_and(|v| v == name)
}
fn main() {'''
try:
    edit('main.rs', 'fn main() {', CONTROL_FN)
    tree = 'reader_code_file/document/data_tree.rs'
    edit(tree, 'aside = attribute("id").or_else(|| attribute("name"));',
         'aside = if crate::tree2813_control("name-before-id") { attribute("name").or_else(|| attribute("id")) } else { attribute("id").or_else(|| attribute("name")) };')
    edit(tree, '.filter(|child| child.kind() == "element" || wrapper(child))',
         '.filter(|child| child.kind() == "element" || wrapper(child) || (crate::tree2813_control("comments-as-rows") && child.kind() == "Comment"))')
    edit(tree, '        if node.kind() == "element" {\n            out.push(node);',
         '        if node.kind() == "element" || node.kind() == "Comment" {\n            out.push(node);')
    edit(tree, '.is_some_and(|first| header(first.node) != header(child))',
         '.is_some_and(|first| crate::tree2813_control("no-toml-merge") || header(first.node) != header(child))')
    edit(tree, '.filter(|node| node.kind() == "document")',
         '.filter(|node| node.kind() == "document").take(if crate::tree2813_control("first-document-only") { 1 } else { usize::MAX })')
    edit(tree, '!child.is_error() && child.kind() != "comment")\n        .last();', '!child.is_error() && (child.kind() != "comment" || crate::tree2813_control("toml-comment-value")))\n        .last();')
    edit(tree, '!matches!(child.kind(), "anchor" | "tag" | "comment")', '!matches!(child.kind(), "anchor" | "tag") && (child.kind() != "comment" || crate::tree2813_control("yaml-comment-content"))')
    edit(tree, "let rest = text[key.byte_range()].strip_prefix(name)?;", "let rest = text[key.byte_range()].strip_prefix(name).filter(|_| !crate::tree2813_control(\"no-subtable-merge\"))?;")
    edit('reader_code_file/structure.rs', 'if Some(index) == Self::pinned_open(doc) && !self.folded(index) {', 'if !crate::tree2813_control("root-toggle-closes") && Some(index) == Self::pinned_open(doc) && !self.folded(index) {')
    doc = 'reader_code_file/document.rs'
    edit(doc, '        let syntax = match (data, &tree) {',
         '        let invalid = if data.is_some() && crate::tree2813_control("grammar-verdict") {\n'
         '            tree.as_ref().and_then(|tree| data_tree::first_syntax_problem(tree, &text, tree_offset))\n'
         '        } else {\n            invalid\n        };\n        let syntax = match (data, &tree) {')
    edit(doc, 'data.is_some() && invalid.is_none() && tree.is_some()',
         'data.is_some() && invalid.is_none() && tree.is_some() && !crate::tree2813_control("tree-json-only")')
    edit(doc, '            semantic,\n            invalid,\n            syntax,\n',
         '            semantic,\n'
         '            invalid: if crate::tree2813_control("syntax-is-invalid") { invalid.or(syntax.clone()) } else { invalid },\n'
         '            syntax: if crate::tree2813_control("syntax-is-invalid") { None } else { syntax },\n')
    edit('reader_code_file/structure.rs', '        if folded {\n            self.exceptions.extend(Self::pinned_open(doc));',
         '        if folded && !crate::tree2813_control("root-folds") {\n            self.exceptions.extend(Self::pinned_open(doc));')
    (OUT / 'mutations.diff').write_text('\n'.join(patches))
    if '--audit' in sys.argv:
        print('TREE2813_MUTATION_ANCHORS_PASS', flush=True)
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
        print('TREE2813_COMPILED_NEGATIVE_FIRED', control, flush=True)
finally:
    for p, data in original.items():
        p.write_bytes(data)
assert all(p.read_bytes() == data for p, data in original.items())
print('TREE2813_RESTORED', flush=True)
