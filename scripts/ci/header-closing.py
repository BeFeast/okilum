#!/usr/bin/env python3
"""Compile #2813 8b closing-pass negatives once; each must fail its named test."""
import difflib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / 'target/header-closing-controls'
OUT.mkdir(parents=True, exist_ok=True)
BASE = ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum',
        '--features', 'data-view-native-harness']
if '--release' in sys.argv:
    BASE.append('--release')
if '--linux-sprint' in sys.argv:
    BASE[BASE.index('data-view-native-harness')] = 'linux-sprint,data-view-native-harness'
if '--shell-no-opt' in sys.argv:
    BASE.extend(['--config', 'profile.release.package.okilum-shell.opt-level=0'])
ENV = {k:v for k,v in os.environ.items() if k != 'OKILUM_CLOSE2813_CONTROL'}
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
    if control: env['OKILUM_CLOSE2813_CONTROL'] = control
    p = subprocess.run(args, cwd=ROOT, env=env, text=True, encoding='utf-8', stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (OUT / (label + '.log')).write_text(p.stdout, encoding='utf-8')
    print(label, p.returncode, p.stdout[-3500:], flush=True)
    return p

CONTROLS = [
 ('fold-unlabelled', 'labels2813_fold_actions_carry_labels_and_yield_first'),
 ('ctrl-alt-letter', 'altgr2813_no_default_uses_ctrl_alt_letter'),
 ('labels-after-overflow', 'labels2813_fold_actions_carry_labels_and_yield_first'),
]
TESTS = ['labels2813_', 'altgr2813_']
CONTROL_FN = '''#[cfg(test)]
fn close2813_control(name: &str) -> bool {
    std::env::var("OKILUM_CLOSE2813_CONTROL").is_ok_and(|v| v == name)
}
fn main() {'''
try:
    edit('main.rs', 'fn main() {', CONTROL_FN)
    edit('reader_code_file/toolbar.rs', '                action.labelled = true;\n                if let Availability::Disabled(reason) = &folding {',
         '                action.labelled = !crate::close2813_control("fold-unlabelled");\n                if let Availability::Disabled(reason) = &folding {')
    edit('main.rs', 'fn bind_keys(cx: &mut App) {', 'fn bind_keys(cx: &mut App) {\n    #[cfg(test)]\n    if crate::close2813_control("ctrl-alt-letter") {\n        cx.bind_keys([KeyBinding::new("ctrl-alt-k", reader_code_file::FoldAll, None)]);\n    }')
    edit('reader_viewer_chrome.rs', '                step.priority = self.collapse_priority;', '                step.priority = if crate::close2813_control("labels-after-overflow") { 60_000 } else { self.collapse_priority };')
    edit('reader_viewer_chrome/overflow.rs', '            steps.push(OverflowStep {\n                priority: 0,\n                width: minimum,', '            steps.push(OverflowStep {\n                priority: if crate::close2813_control("labels-after-overflow") { 60_000 } else { 0 },\n                width: minimum,')
    (OUT / 'mutations.diff').write_text('\n'.join(patches))
    if '--audit' in sys.argv:
        print('CLOSE2813_MUTATION_ANCHORS_PASS', flush=True)
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
        print('CLOSE2813_COMPILED_NEGATIVE_FIRED', control, flush=True)
finally:
    for p, data in original.items():
        p.write_bytes(data)
assert all(p.read_bytes() == data for p, data in original.items())
print('CLOSE2813_RESTORED', flush=True)
