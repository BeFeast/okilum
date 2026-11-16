#!/usr/bin/env python3
"""Split each frame's scheduled Running time into DPC/ISR, sampled on-core work
and unsampled (vCPU absent) time, using the xperf dumper of the same ETL.

Inputs (one census directory): */receipt.json, correlation.json (correlate.py),
reader.xml (tracerpt) and dump.csv (xperf -a dumper). Output: split.json.
Diagnostic only; no frame is relabeled and no budget is changed.
"""
import bisect
import collections
import csv
import json
from pathlib import Path
import re
import statistics
import sys
import xml.etree.ElementTree as ET

sys.path.insert(0, str(Path(__file__).parent))
from correlate import number, stamp  # noqa: E402

PROFILE_INTERVAL_MS = 0.5


def header(path):
    fields = {}
    with path.open(encoding='utf-8', errors='replace') as handle:
        for line in handle:
            if line.startswith('EndHeader'):
                break
            row = [c.strip() for c in next(csv.reader([line]))]
            if len(row) > 1 and row[1] == 'TimeStamp':
                fields[row[0]] = row
    return fields


def col(fields, *names):
    lowered = [f.lower() for f in fields]
    for name in names:
        for i, f in enumerate(lowered):
            if f == name.lower() or f.startswith(name.lower()):
                return i
    return None


def image(value):
    value = value.strip()
    return value.split('!', 1)[0] if '!' in value else value or '?'


def xml_switches(path, tids):
    out = collections.defaultdict(list)
    for _, event in ET.iterparse(path):
        if event.tag.rsplit('}', 1)[-1] != 'Event':
            continue
        if event.findtext('./{*}RenderingInfo/{*}Opcode') == 'CSwitch':
            data = {e.attrib.get('Name'): e.text or '' for e in event.findall('.//{*}Data')}
            old, new = number(data.get('OldThreadId')), number(data.get('NewThreadId'))
            for tid in tids & {old, new}:
                out[tid].append((stamp(event.find('./{*}System/{*}TimeCreated').attrib['SystemTime']), old, new))
        event.clear()
    return out


def main(root):
    root = Path(root)
    fields = header(root / 'dump.csv')
    headers = {name: row for name, row in fields.items()}
    corr = {c['case']: c for c in json.loads((root / 'correlation.json').read_text())}
    tids = {c['tid'] for c in corr.values()}
    cs = fields.get('CSwitch')
    sp = fields.get('SampledProfile')
    assert cs and sp, ('dumper header lacks CSwitch/SampledProfile', sorted(fields))
    c_new, c_old, c_cpu = col(cs, 'New TID'), col(cs, 'Old TID'), col(cs, 'CPU')
    s_tid, s_cpu, s_img = col(sp, 'ThreadID'), col(sp, 'CPU'), col(sp, 'Image!Function')
    interrupt_kinds = {name: row for name, row in fields.items()
                       if re.search(r'DPC|Interrupt|ISR', name, re.I) and col(row, 'ElapsedTime') is not None}
    switches = collections.defaultdict(list)   # tid -> (us, old, new, cpu)
    samples = collections.defaultdict(list)    # cpu -> (us, tid, image)
    interrupts = collections.defaultdict(list)  # cpu -> (start_us, end_us, kind, routine)
    stamp_end = {}
    with (root / 'dump.csv').open(encoding='utf-8', errors='replace') as handle:
        body = False
        for line in handle:
            if not body:
                body = line.startswith('EndHeader')
                continue
            name = line.split(',', 1)[0].strip()
            if name not in ('CSwitch', 'SampledProfile') and name not in interrupt_kinds:
                continue
            row = [c.strip() for c in next(csv.reader([line]))]
            try:
                ts = int(row[1])
            except (ValueError, IndexError):
                continue
            if name == 'CSwitch':
                old, new = number(row[c_old]), number(row[c_new])
                for tid in tids & {old, new}:
                    switches[tid].append((ts, old, new, number(row[c_cpu])))
            elif name == 'SampledProfile':
                samples[number(row[s_cpu])].append((ts, number(row[s_tid]), image(row[s_img])))
            else:
                spec = interrupt_kinds[name]
                elapsed = float(row[col(spec, 'ElapsedTime')])
                cpu = number(row[col(spec, 'CPU')]) if col(spec, 'CPU') is not None else None
                routine = row[col(spec, 'Routine')] if col(spec, 'Routine') is not None else ''
                # xperf dumper prints DPC/ISR rows at their entry time with ElapsedTime in microseconds.
                interrupts[cpu].append((ts, ts + elapsed, name, image(routine)))
                stamp_end[name] = stamp_end.get(name, 0) + 1
    for values in samples.values():
        values.sort()
    for values in interrupts.values():
        values.sort()
    # Map FILETIME brackets onto the dumper's microsecond clock through identical CSwitch sequences.
    xml = xml_switches(root / 'reader.xml', tids)
    offsets = []
    for tid in tids:
        a = [(o, n) for _, o, n in xml[tid]]
        b = [(o, n) for _, o, n, _ in switches[tid]]
        assert a == b, ('CSwitch sequences differ between tracerpt and dumper', tid, len(a), len(b))
        offsets += [x[0] / 10 - y[0] for x, y in zip(xml[tid], switches[tid])]
    offset = statistics.median(offsets)
    spread = max(abs(o - offset) for o in offsets)
    assert spread <= 50, ('clock mapping spread too large (us)', spread)
    results = []
    for case, info in sorted(corr.items()):
        tid = info['tid']
        runs = []  # (start_us, end_us, cpu)
        start = None
        for ts, old, new, cpu in sorted(switches[tid]):
            if new == tid:
                start = (ts, cpu)
            if old == tid and start:
                runs.append((start[0], ts, start[1]))
                start = None
        frames = []
        for frame in info['frames']:
            receipt_bounds = next(f for f in json.loads((root / case / 'receipt.json').read_text())['frames']
                                  if f['marks'] == frame['marks'] and f['sample'] == frame['sample'])
            left, right = receipt_bounds['subtree_work']['native_memory_and_cycles']['wall_bounds_filetime_100ns']
            lo, hi = left / 10 - offset, right / 10 - offset
            running_us = intr_us = 0.0
            own = collections.Counter()
            max_gap_us = 0.0
            routines = collections.Counter()
            expected = 0.0
            for a, b, cpu in runs:
                a, b = max(a, lo), min(b, hi)
                if b <= a:
                    continue
                running_us += b - a
                expected += (b - a) / 1000 / PROFILE_INTERVAL_MS
                for s, e, kind, routine in interrupts.get(cpu, []):
                    overlap = min(e, b) - max(s, a)
                    if overlap > 0:
                        intr_us += overlap
                        routines[f'{kind}:{routine}'] += overlap
                series = samples.get(cpu, [])
                i, j = bisect.bisect_left(series, (a,)), bisect.bisect_right(series, (b, float('inf')))
                points = [a] + [t for t, _, _ in series[i:j]] + [b]
                max_gap_us = max(max_gap_us, max(y - x for x, y in zip(points, points[1:])))
                for _, stid, img in series[i:j]:
                    own[img if stid == tid else f'other-tid:{img}'] += 1
            sampled = sum(own.values())
            frames.append({'marks': frame['marks'], 'sample': frame['sample'], 'wall_ms': frame['wall_ms'],
                           'ready_ms': frame['ready_ms'], 'running_dumper_ms': running_us / 1000,
                           'running_tracerpt_ms': frame['running_bracket_ms'],
                           'dpc_isr_ms': intr_us / 1000,
                           'top_dpc_isr': [[k, v / 1000] for k, v in routines.most_common(5)],
                           'profile_samples': sampled, 'expected_samples': expected,
                           'sampled_fraction': sampled / expected if expected else None,
                           'max_sample_gap_ms_on_cpu': max_gap_us / 1000,
                           'samples_by_image': dict(own.most_common(12))})
        over = [f for f in frames if f['running_tracerpt_ms'] > 16]
        results.append({'case': case, 'tid': tid, 'frames': frames, 'running_over_16': over})
    def med(values):
        values = list(values)
        return statistics.median(values) if values else None

    def summary(kind):
        rows = [f for r in results if r['case'].endswith(kind) for f in r['frames']]
        normal = [f for f in rows if f['running_tracerpt_ms'] <= 16]
        return {'frames': len(rows),
                'running_over_16': sum(f['running_tracerpt_ms'] > 16 for f in rows),
                'wall_over_16': sum(f['wall_ms'] > 16 for f in rows),
                'p50_running_ms': statistics.median(f['running_tracerpt_ms'] for f in rows),
                'max_running_ms': max(f['running_tracerpt_ms'] for f in rows),
                'p50_sampled_fraction_normal': med(
                    f['sampled_fraction'] for f in normal if f['sampled_fraction'] is not None),
                'max_dpc_isr_ms': max(f['dpc_isr_ms'] for f in rows),
                'p50_dpc_isr_ms': med(f['dpc_isr_ms'] for f in rows)}
    out = {'clock_offset_us': offset, 'clock_spread_us': spread, 'profile_interval_ms': PROFILE_INTERVAL_MS,
           'dumper_headers': {k: headers[k] for k in sorted(headers) if k in ('CSwitch', 'SampledProfile') or k in interrupt_kinds},
           'interrupt_rows': stamp_end,
           'summary': {k: summary(k) for k in ('main', 'toolbar')},
           'top_running_frames': sorted((dict(f, case=r['case']) for r in results for f in r['frames']),
                                        key=lambda f: -f['running_tracerpt_ms'])[:15],
           'cases': results}
    (root / 'split.json').write_text(json.dumps(out, indent=2))
    print(json.dumps({k: out[k] for k in ('clock_offset_us', 'clock_spread_us', 'interrupt_rows', 'summary')}, indent=2))
    for f in out['top_running_frames']:
        print(json.dumps({k: f[k] for k in ('case', 'marks', 'sample', 'wall_ms', 'ready_ms', 'running_tracerpt_ms',
                                            'dpc_isr_ms', 'profile_samples', 'expected_samples',
                                            'max_sample_gap_ms_on_cpu', 'samples_by_image')}))


if __name__ == '__main__':
    main(sys.argv[1])
