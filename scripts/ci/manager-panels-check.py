#!/usr/bin/env python3
"""Mounted Reader regressions and compiled old-behavior controls for #2693/#2694."""
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
QA = ROOT / 'qa/manager-panels-2693-2694'
COMMAND = ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum']
WARM = 'project::panel_tests::manager_2693_reader_toggle_rearms_retained_cached_paint_once'
DETAILS = 'project::panel_tests::manager_2694_expanded_details_clip_before_history_at_narrow_short_sizes'


def run(label, name, *args):
    result = subprocess.run(COMMAND + [name, '--', *args], cwd=ROOT,
                            capture_output=True, text=True, timeout=1500)
    QA.mkdir(parents=True, exist_ok=True)
    (QA / f'{label}.log').write_text(result.stdout + result.stderr)
    print(result.stdout, end='', flush=True)
    print(result.stderr, end='', flush=True)
    return result


def inventory(label, name, count=1):
    result = run(label, name, '--list')
    assert result.returncode == 0, 'regression must compile before execution'
    assert len(re.findall(r'^.*: test$', result.stdout, re.M)) >= count, 'nonempty inventory required'


def negative(label, relative, original, replacement, name):
    path = ROOT / relative
    source = path.read_bytes()
    try:
        text = source.decode()
        assert text.count(original) == 1, f'exact production control: {label}'
        path.write_bytes(text.replace(original, replacement).encode())
        inventory(f'{label}-inventory', name)
        result = run(label, name, '--exact', '--nocapture', '--test-threads=1')
        assert result.returncode != 0
        assert re.search(r'test result: FAILED\. 0 passed; 1 failed;', result.stdout), (
            'negative control needs a compiled assertion failure, not infrastructure/compiler failure')
        print(f'MANAGER_PANELS_NEGATIVE_PASS {label}', flush=True)
    finally:
        path.write_bytes(source)
        assert path.read_bytes() == source


def main():
    inventory('fixed-inventory', 'project::panel_tests::', 3)
    assert run('fixed', 'project::panel_tests::', '--nocapture', '--test-threads=1').returncode == 0
    # Restore only the real Reader reopen route; test-side direct activation
    # cannot accidentally make this control pass.
    negative('old-reader-reopen', 'crates/okilum-shell/src/main.rs',
             'project.manager_surface_visible(visible, cx)',
             'project.draft_handoff_surface_visible(visible, cx)', WARM)
    negative('unbounded-details', 'crates/okilum-shell/src/project/native.rs',
             '            .flex_none()\n            .min_h_0()\n'
             '            .max_h(relative(0.4))\n            .overflow_y_scroll()\n', '', DETAILS)
    inventory('restored-inventory', 'project::panel_tests::', 3)
    assert run('restored', 'project::panel_tests::', '--nocapture', '--test-threads=1').returncode == 0
    inventory('retained-ttfm-inventory', 'manager_ttfm_', 3)
    assert run('retained-ttfm', 'manager_ttfm_', '--nocapture', '--test-threads=1').returncode == 0
    print('MANAGER_PANELS_RESTORED_PASS', flush=True)


if __name__ == '__main__':
    main()
