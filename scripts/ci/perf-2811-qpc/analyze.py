#!/usr/bin/env python3
"""Fail-closed raw-QPC scheduler attribution. This never changes a native verdict."""
import argparse
from collections import defaultdict
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import re
import struct

from export_raw import MARKER, RAW_MODE, STACK, THREAD

MAX_BOUNDARY_UNCERTAINTY_MS = Fraction(1, 10)  # evidence validity, not a frame budget


class EvidenceError(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise EvidenceError(message)


def loss_check(meta):
    require(meta['raw_timestamp_mode'] == RAW_MODE, 'raw timestamp mode not established')
    require(meta['clock_type'] == 1 and meta['frequency'] > 0, 'QPC clock not established')
    require(meta['process_trace_status'] == meta['close_trace_status'] == 0, 'ETL export incomplete')
    require(not meta['callback_errors'], 'ETL callback decoder error')
    require(meta['events_lost'] == meta['buffers_lost'] == meta['events_lost_after'] == 0, 'ETW loss')
    require(meta['buffer_loss_samples'] and all(v == 0 for v in meta['buffer_loss_samples']), 'ETW buffer loss')


def decode(records):
    result = {'markers': [], 'switches': [], 'ready': [], 'lifetime': [], 'stacks': []}
    for event in records:
        raw = bytes.fromhex(event['payload_hex'])
        provider, op, version, stamp = event['provider'], event['opcode'], event['version'], event['qpc']
        if provider == MARKER:
            require(event['flags'] & 4, 'marker missing STRING_ONLY flag')
            require(len(raw) % 2 == 0, 'invalid UTF-16 marker length')
            text = raw.decode('utf-16-le').rstrip('\0')
            match = re.fullmatch(r'OKILUM_QPC_V1 id=(\d+) pid=(\d+) tid=(\d+)', text)
            require(match is not None, 'invalid marker payload')
            marker_id, pid, tid = map(int, match.groups())
            require(pid == event['pid'] and tid == event['tid'], 'marker header identity mismatch')
            result['markers'].append({'id': marker_id, 'pid': pid, 'tid': tid, 'qpc': stamp})
        elif provider == THREAD:
            if op == 36:
                require(version in [2, 4] and len(raw) == 24, 'unsupported CSwitch schema')
                new, old = struct.unpack_from('<II', raw)
                result['switches'].append({'qpc': stamp, 'new': new, 'old': old,
                    'reason': raw[12], 'state': raw[14], 'cpu': event['cpu']})
            elif op == 50:
                require(version == 2 and len(raw) == 8, 'unsupported ReadyThread schema')
                result['ready'].append({'qpc': stamp, 'tid': struct.unpack_from('<I', raw)[0],
                                        'cpu': event['cpu'], 'waker_tid': event['tid'], 'waker_pid': event['pid']})
            elif op in [1, 2, 3, 4]:
                require(version in [1, 2, 3] and len(raw) >= 8, 'unsupported Thread lifetime schema')
                pid, tid = struct.unpack_from('<II', raw)
                result['lifetime'].append({'qpc': stamp, 'pid': pid, 'tid': tid, 'opcode': op})
        elif provider == STACK and op == 32:
            require(len(raw) >= 24, 'truncated StackWalk')
            event_qpc, pid, tid = struct.unpack_from('<QII', raw)
            width = 4 if event['flags'] & 0x20 else 8
            require((len(raw) - 16) % width == 0, 'invalid StackWalk length')
            ips = [hex(value[0]) for value in struct.iter_unpack('<I' if width == 4 else '<Q', raw[16:])]
            result['stacks'].append({'event_qpc': event_qpc, 'pid': pid, 'tid': tid,
                                     'cpu': event['cpu'], 'ips': ips})
    return result


def correlate_markers(frames, markers, pid, tid, frequency):
    expected = {}
    previous_end = None
    for frame in frames:
        anchor = frame['subtree_work']['diagnostic_qpc']
        require(anchor['version'] == 1, 'unknown native anchor version')
        first_id = int(frame['marks']) * 96 + frame['sample'] * 2
        for n, side in enumerate(['before', 'after']):
            value = anchor[side]
            require(value['pid'] == pid and value['tid'] == tid, 'native marker identity mismatch')
            require(value['frequency'] == frequency, 'native/kernel QPC frequency mismatch')
            require(value['marker_id'] == first_id + n, 'native marker sequence mismatch')
            bounds = value['event_qpc_bounds']
            require(len(bounds) == 2 and bounds[0] <= bounds[1], 'corrupt marker bracket')
            require(first_id + n not in expected, 'duplicate native marker')
            expected[first_id + n] = bounds
        a, b = anchor['start_qpc_bounds']
        c, d = anchor['end_qpc_bounds']
        require(anchor['before']['event_qpc_bounds'][1] <= a <= b <= c <= d <=
                anchor['after']['event_qpc_bounds'][0], 'frame outside surrounding markers')
        require(previous_end is None or previous_end < a, 'non-monotonic frame brackets')
        previous_end = d
    observed = {}
    for marker in markers:
        if marker['pid'] == pid:
            require(marker['tid'] == tid, 'wrong TID marker')
            require(marker['id'] not in observed, 'duplicate ETW marker')
            observed[marker['id']] = marker['qpc']
    require(set(observed) == set(expected), 'missing/unexpected ETW markers or wrong PID')
    # RAW_TIMESTAMP means both sides already use QPC. Offset constraints are a
    # validation, not a fitted time shift. The intersection must contain ZERO.
    lower = max(observed[key] - bounds[1] for key, bounds in expected.items())
    upper = min(observed[key] - bounds[0] for key, bounds in expected.items())
    require(lower <= upper, 'empty clock offset intersection')
    require(lower <= 0 <= upper, 'raw QPC offset intersection excludes zero')
    return {'offset_interval_qpc': [lower, upper], 'applied_offset_qpc': 0,
            'offset_constraint_width_ms': float(Fraction((upper - lower) * 1000, frequency)),
            'markers': len(observed)}


def thread_intervals(decoded, tid):
    rows = sorted((e for e in decoded['lifetime'] if e['tid'] == tid), key=lambda e: e['qpc'])
    active, intervals = None, []
    for event in rows:
        if event['opcode'] in [1, 3]:
            require(active is None, 'unpaired thread lifetime start')
            active = event
        elif active is not None:
            require(active['pid'] == event['pid'], 'thread lifetime owner changed before End')
            intervals.append({'pid': active['pid'], 'start': active['qpc'], 'end': event['qpc'],
                              'start_opcode': active['opcode'], 'end_opcode': event['opcode']})
            active = None
    return intervals


def scheduler_segments(decoded, pid, tid, first, last):
    intervals = [i for i in thread_intervals(decoded, tid) if i['pid'] == pid and
                 i['start'] < first <= last < i['end'] and i['start_opcode'] == 1 and i['end_opcode'] == 2]
    require(len(intervals) == 1, 'missing primary PID/TID lifetime coverage')
    start, end = intervals[0]['start'], intervals[0]['end']
    all_stacks = defaultdict(list)
    for stack in decoded['stacks']:
        all_stacks[(stack['event_qpc'], stack['cpu'])].append(stack)
    origin_counts = defaultdict(int)
    for origin in decoded['switches'] + decoded['ready']:
        origin_counts[(origin['qpc'], origin['cpu'])] += 1

    def owner_lifetime(owner_pid, owner_tid, stamp):
        intervals = [i for i in thread_intervals(decoded, owner_tid)
                     if i['pid'] == owner_pid and i['start'] <= stamp < i['end']]
        require(len(intervals) == 1, 'missing live StackWalk owner lifetime')

    def stack_for(stamp, cpu, expected_pid, expected_tid):
        records = all_stacks.get((stamp, cpu), [])
        try:
            require(records, 'missing StackWalk at exact QPC/CPU')
            require(origin_counts[(stamp, cpu)] == 1, 'ambiguous StackWalk origin QPC/CPU')
            owners = {(s['pid'], s['tid']) for s in records}
            require(len(owners) == 1, 'ambiguous StackWalk owner')
            owner_pid, owner_tid = next(iter(owners))
            # Some classic headers expose 0xffffffff instead of the waker.
            # An inferred owner additionally needs a complete live interval.
            if expected_pid not in [0, 0xffffffff]:
                require(owner_pid == expected_pid, 'wrong StackWalk PID owner')
            if expected_tid not in [0, 0xffffffff]:
                require(owner_tid == expected_tid, 'wrong StackWalk TID owner')
            owner_lifetime(owner_pid, owner_tid, stamp)
            return ([ip for s in records for ip in s['ips']], [{'pid': owner_pid, 'tid': owner_tid}],
                    {'evidence': 'COMPLETE'})
        except EvidenceError as error:
            # Causal association is separate from proven UI-thread states.
            # Unknown idle/DPC context must not invent an owning thread.
            return [], [], {'evidence': 'NO_EVIDENCE', 'reason': str(error),
                            'unassociated_raw_stacks': records, 'qpc': stamp, 'cpu': cpu}
    ready = sorted((e for e in decoded['ready'] if e['tid'] == tid and start <= e['qpc'] <= end),
                   key=lambda e: e['qpc'])
    require(ready, 'ReadyThread observer did not fire')
    switches = sorted((e for e in decoded['switches'] if tid in [e['old'], e['new']]
                       and start <= e['qpc'] <= end), key=lambda e: e['qpc'])
    require(len(switches) > 4, 'CSwitch observer did not fire')
    segments, running, pending, running_cpu = [], None, None, None
    for event in switches:
        stamp = event['qpc']
        if event['old'] == tid:
            if running is not None:
                require(running_cpu == event['cpu'], 'impossible running CPU migration / uncovered states')
                segments.append({'start': running, 'end': stamp, 'state': 'Running'})
            require(pending is None, 'unpaired switch-out / uncovered states')
            pending, running = event, None
        if event['new'] == tid:
            require(running is None, 'unpaired switch-in / uncovered states')
            if pending is not None:
                out = pending['qpc']
                out_stack, out_owners, out_causal = stack_for(out, pending['cpu'], pid, tid)
                common = {'reason': pending['reason'], 'switch_out_qpc': out,
                          'switch_out_stack': out_stack, 'switch_out_stack_owners': out_owners,
                          'switch_out_causal': out_causal}
                if pending['state'] == 1:
                    segments.append(dict(common, start=out, end=stamp, state='Ready'))
                elif pending['state'] == 5:
                    transitions = [event for event in ready if out <= event['qpc'] <= stamp]
                    require(transitions, 'missing ReadyThread transition / uncovered states')
                    ready_event = transitions[0]
                    transition = ready_event['qpc']
                    waker_stack, owners, causal = stack_for(transition, ready_event['cpu'],
                                                   ready_event['waker_pid'], ready_event['waker_tid'])
                    common.update(ready_qpc=transition,
                        ready_stack=waker_stack, ready_stack_owners=owners, ready_causal=causal,
                        waker_header_identity_available=ready_event['waker_pid'] not in [0, 0xffffffff]
                            and ready_event['waker_tid'] not in [0, 0xffffffff],
                        ready_owner_evidence='unassociated: causal NO_EVIDENCE' if causal['evidence'] != 'COMPLETE' else
                            'known-header-and-lifetime' if
                            ready_event['waker_pid'] not in [0, 0xffffffff] and
                            ready_event['waker_tid'] not in [0, 0xffffffff] else
                            'inferred: unique event-QPC/CPU + StackWalk owner + verified lifetime')
                    segments.extend([dict(common, start=out, end=transition, state='Waiting'),
                                     dict(common, start=transition, end=stamp, state='Ready')])
                else:
                    segments.append(dict(common, start=out, end=stamp, state='Unknown',
                                         old_thread_state=pending['state']))
            running, pending, running_cpu = stamp, None, event['cpu']
    return segments


def overlap_components(segments, left, right):
    components = dict.fromkeys(['Running', 'Ready', 'Waiting', 'Unknown'], 0)
    cursor = left
    details = []
    for segment in segments:
        a, b = max(left, segment['start']), min(right, segment['end'])
        if b <= a:
            continue
        require(a == cursor, 'uncovered/overlapping thread states')
        components[segment['state']] += b - a
        if segment['state'] != 'Running':
            details.append(dict(segment, overlap_qpc=[a, b]))
        cursor = b
    require(cursor == right, 'uncovered thread states')
    return components, details


def frame_analysis(frame, segments, frequency):
    anchor = frame['subtree_work']['diagnostic_qpc']
    a, b = anchor['start_qpc_bounds']
    c, d = anchor['end_qpc_bounds']
    require(a <= b <= c <= d, 'corrupt frame QPC ordering')
    inner_ms, outer_ms = Fraction((c - b) * 1000, frequency), Fraction((d - a) * 1000, frequency)
    wall_ms = Fraction(str(frame['cpu_draw_ms']))
    tick_ms = Fraction(1000, frequency)
    require(inner_ms - tick_ms <= wall_ms <= outer_ms + tick_ms, 'Instant/QPC duration mismatch')
    uncertainty = outer_ms - inner_ms
    require(uncertainty <= MAX_BOUNDARY_UNCERTAINTY_MS, 'excessive frame boundary uncertainty')
    inner, _ = overlap_components(segments, b, c)
    outer, details = overlap_components(segments, a, d)
    require(outer['Unknown'] == 0, 'unknown scheduler states in frame')
    return {'marks': frame['marks'], 'sample': frame['sample'], 'wall_ms': float(wall_ms),
            'wall_miss': wall_ms > 16, 'outer_qpc_bounds': [a, d], 'inner_qpc_bounds': [b, c],
            'boundary_uncertainty_ms': float(uncertainty),
            'outer_minus_native_wall_ms': float(outer_ms - wall_ms),
            'component_ranges_ms': {name: [value * 1000 / frequency, outer[name] * 1000 / frequency]
                                    for name, value in inner.items()},
            'off_cpu_intervals': details,
            'causal_association': 'COMPLETE' if all(
                item.get(key, {'evidence': 'COMPLETE'})['evidence'] == 'COMPLETE'
                for item in details for key in ['switch_out_causal', 'ready_causal']) else 'PARTIAL_NO_EVIDENCE',
            'observer': '2 QPC reads inside original interval; ETW writes/formatting outside; no overhead subtracted'}


def analyze(root):
    cases = json.loads((root / 'cases.json').read_text())
    require(len(cases) == 10 and all(c['complete'] for c in cases), 'incomplete census is NO_EVIDENCE')
    require([(int(c['case'].split('-')[0]), c['kind']) for c in cases] ==
            [(p, k) for p in range(5) for k in ['main', 'toolbar']], 'fixed plan mismatch')
    records = []
    frequencies = set()
    for name in ['kernel', 'markers']:
        path = root / f'{name}.jsonl'
        meta = json.loads(path.with_suffix('.meta.json').read_text())
        loss_check(meta)
        require(hashlib.sha256((root / f'{name}.raw.etl').read_bytes()).hexdigest() == meta['input_sha256'],
                'raw ETL provenance hash mismatch')
        require(hashlib.sha256(path.read_bytes()).hexdigest() == meta['output_sha256'], 'export hash mismatch')
        frequencies.add(meta['frequency'])
        records.extend(json.loads(line) for line in path.read_text().splitlines())
    require(len(frequencies) == 1, 'kernel/user frequency mismatch')
    frequency = frequencies.pop()
    decoded = decode(records)
    output = []
    for case in cases:
        receipt = json.loads((root / case['case'] / 'receipt.json').read_text())
        frames = receipt['frames']
        require(len(frames) == 96, 'incomplete frame set')
        pid, tid = case['pid'], case['primary_tid']
        clock = correlate_markers(frames, decoded['markers'], pid, tid, frequency)
        first = frames[0]['subtree_work']['diagnostic_qpc']['start_qpc_bounds'][0]
        last = frames[-1]['subtree_work']['diagnostic_qpc']['end_qpc_bounds'][1]
        segments = scheduler_segments(decoded, pid, tid, first, last)
        details = [frame_analysis(frame, segments, frequency) for frame in frames]
        row = {'case': case['case'], 'pid': pid, 'tid': tid, 'clock': clock, 'frames': details,
               'base_source': case['base_source'], 'diagnostic_commit': case['diagnostic_commit'],
               'binary_sha256': case['binary_sha256'], 'fixture_sha256': case['fixture_sha256'],
               'misses': [frame for frame in details if frame['wall_miss']],
               'strict_pass': receipt['strict_pass'], 'verdict': 'DIAGNOSTIC_EVIDENCE_ONLY'}
        (root / case['case'] / 'scheduler.json').write_text(json.dumps(row, indent=2))
        output.append(row)
    result = {'evidence': 'COMPLETE', 'production_gate_changed': False,
              'frequency': frequency, 'cases': output,
              'qualification': 'scheduled Running conservatively includes interrupt/DPC time; no gate waiver'}
    (root / 'scheduler.json').write_text(json.dumps(result, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    try:
        analyze(args.directory)
    except Exception as error:
        (args.directory / 'scheduler-error.json').write_text(json.dumps({'evidence': 'NO_EVIDENCE',
            'error': repr(error), 'production_gate_changed': False}, indent=2))
        raise
