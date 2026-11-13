#!/usr/bin/env python3
"""Fixed five alternating pairs. Complete data can still be a strict budget FAIL."""
import argparse
import ast
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import time

from capsule import ROOT, SOURCES, git
from provenance import pe_stack

FIXTURE_SHA = '3111144a60615a0a1870164cfc22dae16d60a5a7c0813c7e3e74d294ad47af88'
PLAN = [(pair, kind) for pair in range(5) for kind in ['main', 'toolbar']]


def fixture():
    script = git('show', SOURCES['toolbar'] + ':scripts/ci/code-toolbar-native-app.py').decode()
    function = next(node for node in ast.parse(script).body
                    if isinstance(node, ast.FunctionDef) and node.name == 'fixture')
    scope = {'json': json}
    exec(compile(ast.Module(body=[function], type_ignores=[]), '<pinned fixture>', 'exec'), scope)
    raw = scope['fixture']().encode()
    assert hashlib.sha256(raw).hexdigest() == FIXTURE_SHA
    return raw


def validate_receipt(receipt, source, exit_code, log):
    assert receipt['source_sha'] == source and receipt['diagnostic_only'] is True
    assert receipt['real_native_platform'] and receipt['mounted'] == 'Reader'
    assert receipt['measurement'] == 'production-release'
    assert receipt['bytes'] == 3500000 and receipt['paths_keys'] == 414
    assert receipt['initial_rasterizations'] > 0
    frames = receipt['frames']
    assert [(f['marks'], f['sample']) for f in frames] == [(m, n) for m in [False, True] for n in range(48)]
    for marked in [False, True]:
        group = [f for f in frames if f['marks'] == marked]
        assert len({f['scroll_y'] for f in group}) == 48
        assert all(f['rows_built'] > 0 and f['present_ms'] >= 0 for f in group)
        assert sum(f['text_work']['work']['glyph_bitmap']['calls'] for f in group) <= 1
    expected = []
    if receipt['open_ms'] > 100:
        expected.append({'kind': 'open', 'wall_ms': receipt['open_ms'], 'limit_ms': 100})
    for frame in frames:
        if frame['cpu_draw_ms'] > 16:
            expected.append({'kind': 'frame', 'marks': frame['marks'], 'sample': frame['sample'],
                             'wall_ms': frame['cpu_draw_ms'], 'limit_ms': 16})
    assert receipt['budget_violations'] == expected, 'every original violation must remain observable'
    assert receipt['strict_pass'] == (not expected)
    marker = f"CODE2811_QPC_CENSUS_COMPLETE frames=96 strict_pass={str(not expected).lower()}"
    assert marker in log
    assert 'CODE2810_NATIVE_READER_PASS' not in log, 'diagnostic cannot masquerade as native gate PASS'
    assert (exit_code == 0) == (not expected), 'a complete diagnostic with violations must exit nonzero'
    if expected:
        assert 'original native budgets failed after complete diagnostic census' in log
    return frames


def launch(binary, env, log_path):
    import _winapi
    import msvcrt
    # Preserve the primary native thread and default PE stack/priority/affinity.
    with log_path.open('wb', buffering=0) as log, open(os.devnull, 'rb') as input_file:
        for stream in [log, input_file]:
            os.set_handle_inheritable(msvcrt.get_osfhandle(stream.fileno()), True)
        startup = subprocess.STARTUPINFO()
        startup.dwFlags |= _winapi.STARTF_USESTDHANDLES
        startup.hStdInput = msvcrt.get_osfhandle(input_file.fileno())
        startup.hStdOutput = msvcrt.get_osfhandle(log.fileno())
        startup.hStdError = startup.hStdOutput
        process, thread, pid, tid = _winapi.CreateProcess(
            str(binary), subprocess.list2cmdline([str(binary)]), None, None, True, 0, env, None, startup)
        _winapi.CloseHandle(thread)
        timed_out = _winapi.WaitForSingleObject(process, 180000) == _winapi.WAIT_TIMEOUT
        if timed_out:
            _winapi.TerminateProcess(process, 1)
            _winapi.WaitForSingleObject(process, 10000)
        code = _winapi.GetExitCodeProcess(process)
        _winapi.CloseHandle(process)
    return pid, tid, code, timed_out


def run_case(inputs, kind, out, text):
    out.mkdir(parents=True, exist_ok=False)
    manifest = json.loads((inputs / kind / 'build-manifest.json').read_text())
    assert manifest['kind'] == kind and manifest['base_source'] == SOURCES[kind]
    binary = (inputs / kind / 'okilum.exe').resolve()
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == manifest['binary_sha256']
    assert pe_stack(binary.read_bytes()) == (1048576, 4096)
    info = subprocess.check_output([str(binary), '--build-info'], text=True)
    assert manifest['diagnostic_commit'] in info and 'gpui-test-support: no' in info
    (out / 'build-info.txt').write_text(info)
    root = out / 'fixture'
    (root / 'vault').mkdir(parents=True)
    path = root / 'vault/api.json'
    path.write_bytes(text)
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(('OKILUM_DIAG_', 'OKILUM_ATLAS_', 'OKILUM_TREE_', 'OKILUM_GLYPH_'))}
    env.pop('OKILUM_CODE_TOOLBAR_NATIVE_SMOKE', None)
    env.update(OKILUM_CODE_TOOLBAR_NATIVE_FIXTURE=str(root), OKILUM_DIAG_COLLECT_ALL='1')
    result = {'kind': kind, 'base_source': SOURCES[kind],
              'diagnostic_commit': manifest['diagnostic_commit'],
              'binary_sha256': manifest['binary_sha256'], 'fixture_sha256': FIXTURE_SHA,
              'creation_flags': 0, 'start_unix_ns': time.time_ns(), 'complete': False}
    pid, tid, code, timeout = launch(binary, env, out / 'native.log')
    result.update(pid=pid, primary_tid=tid, exit_code=code, timed_out=timeout, end_unix_ns=time.time_ns())
    try:
        assert not timeout, 'native diagnostic timed out'
        log = (out / 'native.log').read_text(encoding='utf-8', errors='replace')
        receipt = json.loads((root / 'toolbar-diagnostic-receipt.json').read_text())
        frames = validate_receipt(receipt, manifest['diagnostic_commit'], code, log)
        assert all(f['subtree_work']['diagnostic_qpc']['before']['pid'] == pid and
                   f['subtree_work']['diagnostic_qpc']['before']['tid'] == tid for f in frames)
        assert hashlib.sha256(path.read_bytes()).hexdigest() == FIXTURE_SHA
        result.update(complete=True, strict_pass=receipt['strict_pass'],
                      wall_misses=sum(f['cpu_draw_ms'] > 16 for f in frames), frames=96,
                      phases={str(marked): {
                          'frames': 48, 'misses': sum(f['cpu_draw_ms'] > 16 for f in frames if f['marks'] == marked),
                          'max_wall_ms': max(f['cpu_draw_ms'] for f in frames if f['marks'] == marked)}
                          for marked in [False, True]})
        (out / 'receipt.json').write_text(json.dumps(receipt, indent=2))
    except Exception as error:
        result['error'] = repr(error)
    (out / 'case.json').write_text(json.dumps(result, indent=2))
    return result


def main(inputs, out):
    assert os.name == 'nt'
    inputs, out = inputs.resolve(), out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    (out / 'plan.json').write_text(json.dumps(PLAN))
    (out / 'host.json').write_text(json.dumps({'platform': platform.platform(), 'cpus': os.cpu_count(),
        'runner': os.environ.get('RUNNER_NAME'), 'controller': git('rev-parse', 'HEAD').decode().strip(),
        'thresholds': {'open_ms': 100, 'frame_ms': 16}, 'meaning': 'diagnostic, never a green-gate substitute'}, indent=2))
    results = []
    text = fixture()
    for pair, kind in PLAN:
        result = run_case(inputs, kind, out / f'{pair}-{kind}', text)
        result['case'] = f'{pair}-{kind}'
        results.append(result)
        (out / 'cases.json').write_text(json.dumps(results, indent=2))
        print(json.dumps(result), flush=True)
    complete = all(case['complete'] for case in results)
    summary = {'complete': complete, 'strict_pass': complete and all(c['strict_pass'] for c in results)}
    for kind in SOURCES:
        rows = [case for case in results if case['kind'] == kind]
        summary[kind] = {'launches': len(rows), 'complete': sum(c['complete'] for c in rows),
                         'frames': sum(c.get('frames', 0) for c in rows),
                         'wall_misses': sum(c.get('wall_misses', 0) for c in rows),
                         'failing_launches': sum(c['complete'] and not c['strict_pass'] for c in rows)}
        summary[kind]['phases'] = {str(marked): {
            'frames': sum(c.get('phases', {}).get(str(marked), {}).get('frames', 0) for c in rows),
            'misses': sum(c.get('phases', {}).get(str(marked), {}).get('misses', 0) for c in rows)}
            for marked in [False, True]}
    (out / 'summary.json').write_text(json.dumps(summary, indent=2))
    print(json.dumps(summary, indent=2), flush=True)
    assert complete, 'incomplete census is NO_EVIDENCE'
    assert summary['strict_pass'], 'original budgets FAILED; complete diagnostic data is retained'


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--inputs', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    main(args.inputs, args.out)
