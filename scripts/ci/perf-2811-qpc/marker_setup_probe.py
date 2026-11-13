#!/usr/bin/env python3
"""Setup-only xperf provider roundtrip. Never launches an Okilum binary."""
import argparse
import ctypes as C
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import uuid

from export_raw import Guid, MARKER, U8, U32, U64, export

TOKEN = MARKER + ':0x1:4'
SESSION = 'OkilumFrameQpcSetupProof'
MESSAGE = 'OKILUM_QPC_SETUP_ONLY_1'
XPERF_SHA256 = '6e0c15d303e9369a8756b7f9b63978220ba3c8f9b9471a00b0e63958cc095561'


def command(xperf, token, etl):
    return [xperf, '-start', SESSION, '-on', token, '-ClockType', 'PerfCounter',
            '-BufferSize', '1024', '-MinBuffers', '16', '-MaxBuffers', '64', '-f', str(etl)]


def validate_events(events, emission, meta):
    assert meta['raw_timestamp_mode'] == 0x10001000
    assert meta['clock_type'] == 1 and meta['frequency'] == emission['frequency']
    matches = [v for v in events if v['provider'] == MARKER]
    assert len(matches) == 1, ('expected exactly one synthetic marker', len(matches))
    event = matches[0]
    assert event['pid'] == emission['pid'] and event['tid'] == emission['tid']
    assert emission['before_qpc'] <= event['qpc'] <= emission['after_qpc']
    assert bytes.fromhex(event['payload_hex']).decode('utf-16-le').rstrip('\x00') == MESSAGE
    return event


def emit():
    api = C.WinDLL('advapi32', use_last_error=True)
    kernel = C.WinDLL('kernel32', use_last_error=True)
    api.EventRegister.argtypes = [C.POINTER(Guid), C.c_void_p, C.c_void_p, C.POINTER(U64)]
    api.EventRegister.restype = U32
    api.EventWriteString.argtypes = [U64, U8, U64, C.c_wchar_p]
    api.EventWriteString.restype = U32
    api.EventUnregister.argtypes = [U64]
    api.EventUnregister.restype = U32
    kernel.QueryPerformanceCounter.argtypes = [C.POINTER(C.c_int64)]
    kernel.QueryPerformanceFrequency.argtypes = [C.POINTER(C.c_int64)]
    kernel.GetCurrentThreadId.restype = U32
    provider = Guid()
    provider.bytes[:] = uuid.UUID(MARKER).bytes_le
    handle = U64()
    assert api.EventRegister(C.byref(provider), None, None, C.byref(handle)) == 0
    try:
        before, after, frequency = C.c_int64(), C.c_int64(), C.c_int64()
        assert kernel.QueryPerformanceFrequency(C.byref(frequency))
        assert kernel.QueryPerformanceCounter(C.byref(before))
        status = api.EventWriteString(handle, 4, 1, MESSAGE)
        assert kernel.QueryPerformanceCounter(C.byref(after))
        assert status == 0, ('EventWriteString', status)
        return {'pid': os.getpid(), 'tid': kernel.GetCurrentThreadId(), 'frequency': frequency.value,
                'before_qpc': before.value, 'after_qpc': after.value, 'level': 4, 'keyword': 1}
    finally:
        assert api.EventUnregister(handle) == 0


def main(out):
    assert os.name == 'nt' and C.sizeof(C.c_void_p) == 8
    out.mkdir(parents=True, exist_ok=False)
    xperf = shutil.which('xperf.exe')
    assert xperf, 'Use the installed WPT xperf; do not install or change it in this probe'
    actual_hash = hashlib.sha256(Path(xperf).read_bytes()).hexdigest()
    assert actual_hash == XPERF_SHA256, ('WPT differs from failed setup; no comparable proof', actual_hash)
    receipt = {'benchmark_launches': 0, 'synthetic_probe_only': True, 'xperf': xperf,
               'xperf_sha256': actual_hash, 'session': SESSION, 'token': TOKEN, 'complete': False, 'commands': []}
    def run(args):
        result = subprocess.run(args, capture_output=True, text=True)
        receipt['commands'].append({'argv': args, 'exit': result.returncode,
                                    'stdout': result.stdout, 'stderr': result.stderr})
        (out / 'proof.json').write_text(json.dumps(receipt, indent=2))
        return result
    try:
        # Reproduce the failed spelling before proving the corrected command.
        negative = run(command(xperf, '{' + MARKER + '}:0x1:4', out / 'braced-negative.etl'))
        if negative.returncode == 0:
            run([xperf, '-stop', SESSION])
            raise AssertionError('Braced spelling unexpectedly succeeded; hypothesis unproven')
        assert 'Invalid flags' in negative.stdout + negative.stderr, negative
        positive = run(command(xperf, TOKEN, out / 'synthetic.raw.etl'))
        assert positive.returncode == 0, positive
        try:
            emission = emit()
            receipt['emission'] = emission
        finally:
            stopped = run([xperf, '-stop', SESSION])
            assert stopped.returncode == 0, stopped
        exported = out / 'synthetic.jsonl'
        export(out / 'synthetic.raw.etl', exported, {emission['tid']})
        events = [json.loads(line) for line in exported.read_text().splitlines()]
        meta = json.loads(exported.with_suffix('.meta.json').read_text())
        receipt['decoded_marker'] = validate_events(events, emission, meta)
        receipt['raw_meta'] = meta
        receipt['complete'] = True
    except BaseException as error:
        receipt['error'] = repr(error)
        raise
    finally:
        (out / 'proof.json').write_text(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', required=True, type=Path)
    main(parser.parse_args().out)
