#!/usr/bin/env python3
"""Mac UI-thread CPU query deltas only; no scheduler-state claim or gate waiver."""
import argparse
import json
import math
from pathlib import Path


def frame_cpu(frame, pid, tid):
    sample = frame['subtree_work']['diagnostic_macos_cpu']
    clock = sample['clock']
    assert clock['api'] == 'CLOCK_THREAD_CPUTIME_ID' and clock['clock_id'] == 16
    assert clock['pid'] == pid and clock['native_tid'] == tid and clock['main_thread'] is True
    assert clock['resolution_ns'] > 0 and clock['pthread_stack_bytes'] > 0
    before, after = sample['before_cpu_ms'], sample['after_cpu_ms']
    bracket, wall = sample['cpu_bracket_wall_ms'], frame['cpu_draw_ms']
    assert all(math.isfinite(value) for value in [before, after, bracket, wall])
    assert 0 <= before <= after and bracket >= wall
    delta = after - before
    assert abs(delta - frame['thread_cpu_ms']) <= 1e-6
    assert abs(bracket - wall - sample['cpu_bracket_margin_ms']) <= 1e-6
    assert delta <= bracket + .1, 'thread clock delta inconsistent with enclosing wall bracket'
    return {'marks': frame['marks'], 'sample': frame['sample'], 'wall_ms': wall,
            'wall_miss': wall > 16, 'thread_cpu_query_delta_ms': delta,
            'cpu_bracket_wall_ms': bracket, 'cpu_bracket_margin_ms': bracket - wall,
            'resolution_ns': clock['resolution_ns'], 'pid': pid, 'native_tid': tid,
            'main_thread': True, 'pthread_stack_bytes': clock['pthread_stack_bytes'],
            'limitation': 'CPU query bracket includes bookkeeping outside the original frame. No Ready/Waiting attribution.'}


def analyze(root):
    assert json.loads((root / 'postflight.json').read_text())['immutable_binaries_verified'] is True
    cases = json.loads((root / 'cases.json').read_text())
    assert len(cases) == 10 and all(case['complete'] for case in cases)
    assert [(int(c['case'].split('-')[0]), c['kind']) for c in cases] == \
           [(p, k) for p in range(5) for k in ['main', 'toolbar']]
    stacks, details = set(), []
    for case in cases:
        receipt = json.loads((root / case['case'] / 'receipt.json').read_text())
        assert len(receipt['frames']) == 96
        rows = [frame_cpu(frame, case['pid'], case['primary_tid']) for frame in receipt['frames']]
        assert any(row['thread_cpu_query_delta_ms'] > 0 for row in rows), 'CPU observer positive control'
        samples = [frame['subtree_work']['diagnostic_macos_cpu'] for frame in receipt['frames']]
        assert all(b['before_cpu_ms'] >= a['after_cpu_ms'] for a, b in zip(samples, samples[1:]))
        stacks.update(row['pthread_stack_bytes'] for row in rows)
        details.append({'case': case['case'], 'base_source': case['base_source'],
                        'diagnostic_commit': case['diagnostic_commit'], 'binary_sha256': case['binary_sha256'],
                        'strict_pass': receipt['strict_pass'], 'frames': rows,
                        'misses': [row for row in rows if row['wall_miss']]})
    assert len(stacks) == 1, 'main/head must retain the same inherited default native main stack'
    report = {'evidence': 'THREAD_CPU_QUERY_DELTAS_ONLY', 'production_gate_changed': False,
              'pthread_stack_bytes': stacks.pop(), 'cases': details,
              'limitation': 'CPU alone cannot distinguish Ready from Waiting; no scheduler-state attribution or wall-gate waiver'}
    (root / 'mac-thread-cpu.json').write_text(json.dumps(report, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    try:
        analyze(args.directory)
    except Exception as error:
        (args.directory / 'mac-cpu-error.json').write_text(json.dumps({'evidence': 'NO_EVIDENCE', 'error': repr(error)}))
        raise
