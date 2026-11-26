#!/usr/bin/env python3
"""Small data-driven Linux acceptance runner, using the existing Wayland rig."""
import argparse
import base64
from contextlib import contextmanager
from datetime import datetime, timedelta
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import time
import unicodedata
from urllib.parse import quote
from zoneinfo import ZoneInfo

ROOT = Path(__file__).resolve().parent.parent
MAX_STDOUT_BYTES = 4096


def _utf8_tail(text, max_bytes):
    if max_bytes <= 0 or not text:
        return ''
    raw = text.encode('utf-8')
    if len(raw) <= max_bytes:
        return text
    sliced = raw[-max_bytes:]
    idx = 0
    while idx < len(sliced) and (sliced[idx] & 0xC0) == 0x80:
        idx += 1
    decoded = sliced[idx:].decode('utf-8', 'replace')
    nl = decoded.find('\n')
    if 0 <= nl < len(decoded) - 1:
        return decoded[nl + 1:]
    return decoded


def _read_tail_text(path, tail_bytes=65536):
    if not path.is_file():
        return ''
    size = path.stat().st_size
    with path.open('rb') as fh:
        if size > tail_bytes:
            fh.seek(-tail_bytes, os.SEEK_END)
        data = fh.read()
    idx = 0
    while idx < len(data) and (data[idx] & 0xC0) == 0x80:
        idx += 1
    return data[idx:].decode('utf-8', 'replace')


def format_verify_summary(scenario, exit_code, wall_seconds, output_dir, log_path, max_bytes=MAX_STDOUT_BYTES):
    status = 'PASS' if exit_code == 0 else 'FAIL'
    lines = [
        f'verify: scenario={scenario} status={status} exit_code={exit_code} wall_seconds={wall_seconds}',
        f'log={log_path}',
        f'Evidence: {output_dir}',
    ]
    result_path = output_dir / 'result.json'
    if result_path.is_file():
        try:
            res = json.loads(result_path.read_text(encoding='utf-8'))
            summary_parts = [
                f'passed={res.get("passed")}',
                f'host={res.get("host", "-")}',
                f'verify_seconds={res.get("verify_seconds", "-")}',
            ]
            if res.get('error'):
                summary_parts.append(f'error={res["error"]}')
            lines.append('result: ' + ' '.join(summary_parts))
        except (OSError, json.JSONDecodeError):
            pass
    header = '\n'.join(lines) + '\n'
    header_bytes = len(header.encode('utf-8'))
    if header_bytes >= max_bytes:
        raw = header.encode('utf-8')[:max_bytes]
        while raw and (raw[-1] & 0xC0) == 0x80:
            raw = raw[:-1]
        return raw.decode('utf-8', 'ignore')
    if exit_code == 0:
        return header
    tail_raw = _read_tail_text(log_path)
    if not tail_raw:
        return header
    marker = '... [tail of verify.log]\n'
    budget = max_bytes - header_bytes - len(marker.encode('utf-8'))
    if budget <= 0:
        return header
    tail = _utf8_tail(tail_raw, budget)
    return header + marker + tail


def command(*args, **kwargs):
    return subprocess.run(list(map(str, args)), check=True, **kwargs)


def capture(*args):
    return command(*args, capture_output=True, text=True).stdout


def require(condition, details):
    if not condition:
        raise AssertionError(details)


@contextmanager
def check_gate(step, check, result):
    """A named gate can fail while other rehearsal gates still produce evidence."""
    gate = step.get('gate')
    if gate:
        result.setdefault('gates', {}).setdefault(gate, True)
    try:
        yield
    except AssertionError as error:
        if not gate:
            raise
        check['error'] = str(error)
        result['gates'][gate] = False



def trace_events(root):
    events = []
    for path in (root / 'home').rglob('reader-diagnostic.log'):
        for line in path.read_text().splitlines():
            try:
                events.append(json.loads(line))
            except json.JSONDecodeError:
                pass  # The diagnostic writer may be between writes.
    return events


def pixels(path, region, expression):
    x, y, width, height = region
    # ImageMagick 7.1.1 also emits "list" before the numeric -fx readout.
    return float(capture('convert', path, '-crop', f'{width}x{height}+{x}+{y}',
                         '+repage', '-fx', expression, '-format', '%[fx:mean*w*h]', 'info:').splitlines()[-1])


def text(path, region):
    x, y, width, height = region
    image = command('convert', path, '-crop', f'{width}x{height}+{x}+{y}',
                    '+repage', '-resize', '200%', 'png:-', capture_output=True).stdout
    return command('tesseract', 'stdin', 'stdout', '--psm', '6', input=image,
                   capture_output=True).stdout.decode()



def words(path, region):
    """OCR words with their boxes in screenshot pixels: [{text, left, top, right, bottom}]."""
    x, y, width, height = region
    image = command('convert', path, '-crop', f'{width}x{height}+{x}+{y}',
                    '+repage', '-resize', '200%', 'png:-', capture_output=True).stdout
    tsv = command('tesseract', 'stdin', 'stdout', '--psm', '6', 'tsv', input=image,
                  capture_output=True).stdout.decode()
    found = []
    for row in tsv.splitlines()[1:]:
        cols = row.split('\t')
        if len(cols) < 12 or not cols[11].strip():
            continue
        left, top, w, h = (int(v) // 2 for v in cols[6:10])
        found.append({'text': cols[11].strip(), 'left': x + left, 'top': y + top,
                      'right': x + left + w, 'bottom': y + top + h})
    return found


def word_gap(found, rule):
    """Distance from word `a` to word `b` (first exact OCR match each): on `axis: y`
    (default) top(b) - top(a), on `axis: x` left(b) - right(a)."""
    def box(name):
        match = next((w for w in found if w['text'].strip('.,:;') == name), None)
        require(match is not None, {'layout_missing_word': name, 'words': [w['text'] for w in found]})
        return match
    a, b = box(rule['a']), box(rule['b'])
    return b['left'] - a['right'] if rule.get('axis', 'y') == 'x' else b['top'] - a['top']


def check_layout(found, rules, check):
    """`layout:` rules over OCR boxes: {a, b, axis, min, max, more_than, less_than}.
    `more_than`/`less_than` are {a, b, axis, by}: the gap must be at least
    control + by (default 1), or below control + by (default 0)."""
    check['layout'] = []
    for rule in rules:
        gap = word_gap(found, rule)
        entry = {'a': rule['a'], 'b': rule['b'], 'axis': rule.get('axis', 'y'), 'gap': gap}
        check['layout'].append(entry)
        if 'min' in rule:
            require(gap >= rule['min'], check)
        if 'max' in rule:
            require(gap <= rule['max'], check)
        if 'more_than' in rule:
            base = word_gap(found, rule['more_than'])
            entry['control_gap'] = base
            require(gap >= base + rule['more_than'].get('by', 1), check)
        if 'less_than' in rule:
            base = word_gap(found, rule['less_than'])
            entry['control_gap_less'] = base
            require(gap < base + rule['less_than'].get('by', 0), check)

SENTENCES = (
    'The river carried the morning light past the old stone bridge and into the quiet harbor.',
    'Mara opened her notebook and recorded the small changes that another traveler might overlook.',
    'Beyond the garden wall, a narrow path followed the fields toward a village among the hills.',
    'The keeper set a lantern beside the window while the last boat returned from the eastern shore.',
)
MIB = 1024 * 1024


def fixture_size(spec):
    return int(spec['bytes']) if 'bytes' in spec else int(float(spec['mib']) * MIB)


def generate_markdown(spec):
    """Exactly N ASCII bytes: `# first_line` opens it, `last_line` is the final line."""
    size = fixture_size(spec)
    head = f"# {spec.get('first_line', 'Generated note')}\n\n"
    tail = f"{spec['last_line']}\n" if 'last_line' in spec else ''
    chunks, used, index = [head], len(head), 0
    while True:
        remaining = size - used - len(tail)
        require(remaining >= 0, f'Fixture {spec["path"]} is smaller than its markers')
        if remaining == 0:
            break
        if index % 40 == 0:
            heading = f'## Passage {index // 40 + 1:05d}\n\n'
            if remaining >= len(heading) + 12:
                chunks.append(heading)
                used += len(heading)
                remaining -= len(heading)
        # Whole paragraphs while room remains; the last one absorbs the exact remainder.
        length = remaining if remaining < 300 else 240
        if length < 4:  # too small for a paragraph: pad with blank lines
            chunks.append('\n' * length)
            used += length
            continue
        text = ' '.join(SENTENCES[index % 4:] + SENTENCES[:index % 4])
        text = (text * (length // len(text) + 1))[:length - 3] + '.\n\n'
        chunks.append(text)
        used += len(text)
        index += 1
    data = ''.join(chunks) + tail
    require(len(data) == size, f'Generated {len(data)} bytes, wanted {size}')
    return data.encode('ascii')


def generate_json(spec):
    """Exactly N ASCII bytes of one JSON array, one record per line. `valid: false`
    puts a trailing comma (error_at: end) or a bare word (error_at: middle) in it."""
    size = fixture_size(spec)
    valid = spec.get('valid', True)
    if spec.get('single_line'):
        sep, opener = ',', '['
    else:
        sep, opener = ',\n', '[\n'
    closer = '\n]\n' if not spec.get('single_line') else ']\n'
    records, used, index = [], len(opener) + len(closer), 0
    while True:
        record = json.dumps({'id': index, 'name': f'record-{index:07d}', 'tags': ['alpha', 'beta', index % 7],
                             'note': SENTENCES[index % 4], 'nested': {'ok': True, 'ratio': index / 8}})
        if used + len(record) + len(sep) + 40 > size:
            break
        records.append(record)
        used += len(record) + len(sep)
        index += 1
    # The last record is padded to land on the exact size.
    pad = size - used - len('{"id": -1, "pad": ""}')
    require(pad >= 0, f'Fixture {spec["path"]} too small')
    records.append(json.dumps({'id': -1, 'pad': 'x' * pad}))
    body = sep.join(records)
    if not valid:
        if spec.get('error_at', 'end') == 'middle':
            at = body.find(sep, len(body) // 2)
            body = body[:at] + sep + 'broken' + body[at:]
            # keep the size exact by trimming the final record's padding
            body = body.replace('x' * (len('broken') + len(sep)), '', 1)
        else:
            body += ','
            body = body.replace('x', '', 1)
    data = (opener + body + closer).encode('ascii')
    require(len(data) == size, f'Generated {len(data)} bytes, wanted {size}')
    return data


def generate_tar_gz(spec):
    """Small synthetic `.tar.gz` archive built from `{path, text}` entries."""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode='w:gz') as archive:
        for entry in spec.get('entries', ()):
            data = entry.get('text', '').encode('utf-8')
            info = tarfile.TarInfo(name=entry['path'])
            info.size = len(data)
            info.mtime = int(entry.get('mtime', 1730000000))
            archive.addfile(info, io.BytesIO(data))
    return buf.getvalue()


def write_fixtures(vault, specs):
    for spec in specs:
        path = spec['path']
        # `path_form: nfd` stores the name decomposed, as macOS and Syncthing
        # copies do (`й` as `и` + U+0306), while note text stays composed.
        form = spec.get('path_form')
        if form is not None:
            require(form in ('nfc', 'nfd'), f'Unknown path_form: {form}')
            path = unicodedata.normalize(form.upper(), path)
            require(form == 'nfc' or path != unicodedata.normalize('NFC', path),
                    f'path_form nfd leaves this path unchanged: {spec["path"]}')
        target = vault / path
        require(not Path(path).is_absolute() and '..' not in Path(path).parts,
                f'Fixture path must stay inside the vault: {spec["path"]}')
        target.parent.mkdir(parents=True, exist_ok=True)
        kind = spec['kind']
        if kind == 'text':
            target.write_text(spec['text'], encoding='utf-8')
            continue
        if kind == 'tar_gz':
            target.write_bytes(generate_tar_gz(spec))
            continue
        require(kind in ('markdown', 'json'), f'Unknown fixture kind: {kind}')
        target.write_bytes(generate_markdown(spec) if kind == 'markdown' else generate_json(spec))


def numbered_paths(spec):
    return [spec['path'].format(i=i) for i in range(spec['count'])]


def move_vault_fixture(vault, spec, directories=None):
    """Move files and apply the caller's fixture link rewrites."""
    moves = dict(spec.get('paths', {}))
    for batch in spec.get('numbered', ()):
        sources = numbered_paths({'path': batch['from'], 'count': batch['count']})
        targets = numbered_paths({'path': batch['to'], 'count': batch['count']})
        moves.update(zip(sources, targets, strict=True))
    directories = directories or {}
    moves.update(directories)
    for source, target in moves.items():
        before, after = (vault / source).resolve(), (vault / target).resolve()
        require(before.is_relative_to(vault) and after.is_relative_to(vault)
                and (before.is_dir() if source in directories else before.is_file())
                and not after.exists(), f'Invalid move: {source} → {target}')
        after.parent.mkdir(parents=True, exist_ok=True)
        before.rename(after)
    for rewrite in spec.get('rewrite', ()):
        replace_vault_text(vault, rewrite)
    return moves


def cutover_fixture(archive, spec):
    """Publish a synthetic Grove copy and mark the retained Brain archive."""
    grove = archive.parent / 'grove'
    shutil.copytree(archive, grove)
    opaque = {
        old: (action, new)
        for action in ('keep-nested', 'keep-unit')
        for old, new in spec.get(action, {}).items()
    }
    moves = move_vault_fixture(grove, spec, {old: new for old, (_, new) in opaque.items()})
    grove_id = '550e8400-e29b-41d4-a716-446655440000'
    archive_id = 'cc480ea3-0b98-4783-960f-3a64fa4c274c'
    for vault, identity, lifecycle in (
        (grove, grove_id, {'state': 'active'}),
        (archive, archive_id, {
            'state': 'archive', 'read_only': True,
            'successor': {'vault_id': grove_id, 'name': 'grove'},
            'path_map': {'command': 'brain where', 'note': '@views/path-map.md'},
        }),
    ):
        create_vault_text(vault, {
            'path': '.okilum/link-identity.json',
            'source': json.dumps({'version': 1, 'id': identity}),
        })
        create_vault_text(vault, {
            'path': '.okilum/vault.json',
            'source': json.dumps({'version': 1, 'lifecycle': lifecycle}),
        })
    for old in spec.get('retired', ()):
        (grove / old).unlink()
        moves[old] = None
    for directory, _, _ in os.walk(grove, topdown=False):
        folder = Path(directory)
        if folder != grove and not any(folder.iterdir()):
            folder.rmdir()
    create_vault_text(grove, {
        'path': '@views/path-map.jsonl',
        'source': ''.join(json.dumps({
            'old_path': old, 'new_path': new,
            'action': opaque[old][0] if old in opaque else ('move' if new else 'retire'),
            'rule': 'fixture', 'reason': '', 'aliases': [],
        }) + '\n' for old, new in moves.items()),
    })
    create_vault_text(grove, {
        'path': '@views/path-map.md',
        'source': '# Migration map\n\n[Path map](path-map.jsonl)\n',
    })
    create_vault_text(archive, {'path': '.brain-read-only', 'source': 'Archived'})
    for directory, _, files in os.walk(archive, topdown=False):
        for name in files:
            (Path(directory) / name).chmod(0o444)
        Path(directory).chmod(0o555)
    require(not os.access(archive, os.W_OK), 'The fixture Brain archive is writable')
    return grove


def copy_vault_fixture(vault, fixture):
    """`vault: <repo-relative dir>`: copy that fixture tree into the isolated vault."""
    if fixture is None:
        return
    relative = Path(fixture)
    require(not relative.is_absolute() and '..' not in relative.parts,
            f'Vault fixture must be a repo-relative path: {fixture}')
    source = (ROOT / relative).resolve()
    require(source.is_dir() and source.is_relative_to(ROOT), f'Vault fixture is not a repo directory: {fixture}')
    shutil.copytree(source, vault, dirs_exist_ok=True)


def apply_mtimes(vault, specs, result):
    """`mtime: {vault-relative path: days_ago}`: set file mtimes after the vault is built."""
    now = time.time()
    applied = {}
    for name, days in specs.items():
        relative = Path(name)
        require(not relative.is_absolute() and '..' not in relative.parts,
                f'mtime path must stay inside the vault: {name}')
        target = vault / relative
        require(target.is_file(), f'mtime path is not a file in the vault: {name}')
        stamp = now - float(days) * 86400
        os.utime(target, (stamp, stamp))
        actual = target.stat().st_mtime
        require(abs(actual - stamp) < 2, f'mtime not applied to {name}: {actual} != {stamp}')
        day = datetime.fromtimestamp(actual, ZoneInfo('Asia/Jerusalem')).date().isoformat()
        applied[name] = {'days_ago': days, 'age_days_observed': round((now - actual) / 86400, 3),
                         'jerusalem_day': day}
        print(f'verify: mtime {name} = {days} days ago (observed {applied[name]["age_days_observed"]})', flush=True)
    if applied:
        result['vault_mtimes'] = applied


def rebase_jsonl_dates(vault, specs):
    """Move fixture timestamps together so their latest row is the given age."""
    for name, days in specs.items():
        target = (vault / name).resolve()
        require(target.is_relative_to(vault), f'JSONL path must stay inside the vault: {name}')
        rows = [json.loads(line) for line in target.read_text().splitlines() if line.strip()]
        stamps = [datetime.fromisoformat(row['ts']) for row in rows]
        latest = max(stamps)
        shift = datetime.now(latest.tzinfo) - timedelta(days=days) - latest
        for row, stamp in zip(rows, stamps):
            row['ts'] = (stamp + shift).isoformat(timespec='seconds')
        target.write_text(''.join(json.dumps(row) + '\n' for row in rows))


def replace_vault_text(vault, spec):
    target = (vault / spec['path']).resolve()
    require(target.is_relative_to(vault), f'Path must stay inside the vault: {spec["path"]}')
    source = target.read_text()
    before = spec['before']
    require(before and source.count(before) == 1, f'Expected one match in {spec["path"]}: {before!r}')
    target.write_text(source.replace(before, spec['after'], 1))


def create_vault_text(vault, spec):
    target = (vault / spec['path']).resolve()
    require(target.is_relative_to(vault), f'Path must stay inside the vault: {spec["path"]}')
    target.parent.mkdir(parents=True, exist_ok=True)
    with target.open('x', encoding='utf-8') as stream:
        stream.write(spec['source'])


def remove_vault_file(vault, relative):
    target = vault / relative
    require(target.resolve().is_relative_to(vault), f'Path must stay inside the vault: {relative}')
    target.unlink()


def check_declared_tree_roots(vault, visible, check):
    """Check physical root rows against the copied vault's layout descriptor."""
    descriptor = json.loads((vault / '.okilum/vault.json').read_text())
    declared = set(descriptor['layout']['roots'].values())
    require(declared and all(Path(name).name == name for name in declared),
            'The declared_tree_roots check requires top-level directory names')
    existing = {name for name in declared if (vault / name).is_dir()}
    shown = declared.intersection(line.strip() for line in visible.splitlines())
    check['existing_declared_roots'] = sorted(existing)
    check['visible_declared_roots'] = sorted(shown)
    require(shown == existing, check)


def condition(path, rule, default_region):
    """Does the screenshot satisfy a step's `text` and/or `pixels` rule? (ok, details)"""
    details, ok = {}, True
    if 'text' in rule:
        details['text'] = text(path, rule.get('region', default_region))
        ok = ok and rule['text'] in details['text']
    if 'pixels' in rule:
        spec = rule['pixels']
        details['pixels'] = pixels(path, spec['region'], spec['expression'])
        ok = ok and spec['min'] <= details['pixels'] <= spec['max']
    return ok, details


def act(rig, spec):
    """Deliver a step's keys/click without settling sleeps; returns the time just
    before the final input event is sent."""
    if 'click' in spec:
        wlr = lambda *a: subprocess.run(['wlrctl', 'pointer', *map(str, a)], env=rig.env, check=True)
        wlr('move', -10000, -10000)
        wlr('move', *spec['click'])
        time.sleep(.3)
        started = time.monotonic()
        wlr('click', 'left')
        return started
    args = []
    for m in spec.get('mods', ()):
        args += ['-M', m]
    for name in spec['keys']:
        args += ['-k', name]
    for m in reversed(spec.get('mods', ())):
        args += ['-m', m]
    started = time.monotonic()
    subprocess.run(['wtype', *args], env=rig.env, check=True)
    return started


def grab(rig, path):
    start = time.monotonic()
    subprocess.run(['grim', '-t', 'ppm', str(path)], env=rig.env, check=True)
    return start, time.monotonic(), path


def timed_step(rig, step, scenario, output, scratch, check):
    """Measure action -> condition. Frames are grabbed back to back with their
    timestamps and read (OCR) only afterwards, so reading costs no latency.
    Reported `ms` is when the grab that first showed the condition finished: an
    upper bound (`ms_lower` is when that grab started). The condition must be
    false in a frame taken before the action, or an unfired action would pass."""
    name, region = step['id'], scenario['region']
    require('text' in step or 'pixels' in step, f'Timed step {name} needs a text or pixels condition')
    probe = step.get('probe')
    cache = {}

    def holds(frame, rule):
        key = (frame[2], id(rule))
        if key not in cache:
            cache[key] = condition(frame[2], rule, region)
        return cache[key]

    baseline = grab(rig, scratch / f'{name}-base.ppm')
    require(not holds(baseline, step)[0], {'id': name, 'error': 'condition already true before the action'})
    if probe:
        require(not holds(baseline, probe)[0], {'id': name, 'error': 'probe condition already true before the action'})
    t0 = act(rig, step)
    window = max(step['max_ms'] * 1.5, step['max_ms'] + 500) / 1000
    if probe:
        window = max(window, probe.get('after_ms', 0) / 1000 + max(probe['max_ms'] * 1.5, probe['max_ms'] + 500) / 1000)
    frames, t1 = [], None
    while time.monotonic() - t0 < window:
        if probe and t1 is None and (time.monotonic() - t0) * 1000 >= probe.get('after_ms', 0):
            t1 = act(rig, probe)
        frames.append(grab(rig, scratch / f'{name}-{len(frames):03d}.ppm'))
    # Identical consecutive frames need one reading.
    digests = {f[2]: hashlib.md5(f[2].read_bytes()).hexdigest() for f in frames}
    by_digest = {}

    def reads(frame, rule):
        key = (digests[frame[2]], id(rule))
        if key not in by_digest:
            by_digest[key] = condition(frame[2], rule, region)
        return by_digest[key]

    def first(rule, since):
        for frame in frames:
            if frame[0] >= since:
                ok, details = reads(frame, rule)
                if ok:
                    return frame, details
        return None, None

    frame, details = first(step, t0)
    shot = output / f'{name}.png'
    command('convert', (frame or frames[-1])[2], shot)
    check['screenshot'] = str(shot)
    check['max_ms'] = step['max_ms']
    if frame is None:
        check['text'] = reads(frames[-1], step)[1]
        raise AssertionError({**check, 'error': f'condition not seen within {round(window * 1000)} ms'})
    check.update(details)
    check['ms'] = round((frame[1] - t0) * 1000)
    check['ms_lower'] = round((frame[0] - t0) * 1000)
    require(check['ms'] <= step['max_ms'], check)
    if probe:
        pframe, pdetails = first(probe, t1)
        check['probe'] = {'after_ms': probe.get('after_ms', 0), 'max_ms': probe['max_ms'],
                          'issued_ms': round((t1 - t0) * 1000)}
        if pframe is None:
            command('convert', frames[-1][2], output / f'{name}-probe.png')
            raise AssertionError({**check, 'error': 'probe effect never appeared'})
        command('convert', pframe[2], output / f'{name}-probe.png')
        check['probe'].update(pdetails)
        check['probe']['ms'] = round((pframe[1] - t1) * 1000)
        check['probe']['ms_lower'] = round((pframe[0] - t1) * 1000)
        require(check['probe']['ms'] <= probe['max_ms'], check)
    for path in scratch.glob(f'{name}-*.ppm'):
        path.unlink()
    return shot


def run(args, scenario, output, result):
    from native_binary_guard import require_app_binary
    from wayland_rig import Rig

    if not args.binary:
        if not os.environ.get('CARGO_TARGET_DIR') or os.environ.get('BUILD_SLOT_PROJECT') != 'okilum':
            raise RuntimeError('Builds require a build slot (CARGO_TARGET_DIR missing)')
        start = time.monotonic()
        command('bash', 'scripts/vendor-setup.sh', cwd=ROOT)
        command('cargo', 'build', '-p', 'okilum-shell', '--bin', 'okilum', cwd=ROOT)
        result['build_seconds'] = round(time.monotonic() - start, 3)
        if args.measure_incremental:
            source = ROOT / 'crates/okilum-shell/src/ui_zoom.rs'
            original = source.read_bytes()
            try:
                needle = b'pub(crate) fn init(cx: &mut App) {\n'
                require(original.count(needle) == 1, 'Incremental probe insertion point moved')
                source.write_bytes(original.replace(needle, needle + b'    std::hint::black_box(());\n'))
                start = time.monotonic()
                command('cargo', 'build', '-p', 'okilum-shell', '--bin', 'okilum', cwd=ROOT)
                result['one_line_incremental_seconds'] = round(time.monotonic() - start, 3)
            finally:
                source.write_bytes(original)
        args.binary = str(Path(os.environ['CARGO_TARGET_DIR']) / 'debug/okilum')
    binary = str(Path(args.binary).resolve())
    try:
        require_app_binary(binary)
    except SystemExit as error:
        raise RuntimeError(f'Native binary guard refused the app (exit {error.code})') from error
    with open(binary, 'rb') as stream:
        result['binary_sha256'] = hashlib.file_digest(stream, 'sha256').hexdigest()
    for tool in ('sway', 'wtype', 'wlrctl', 'grim', 'dbus-launch', 'convert', 'tesseract'):
        if not shutil.which(tool):
            raise RuntimeError(f'Missing Linux acceptance dependency: {tool}')
    with tempfile.TemporaryDirectory(prefix='okilum-verify-') as scratch:
        root = Path(scratch)
        vault = root / ('brain' if any('cutover' in step for step in scenario['steps']) else 'vault')
        if args.scratch_vault is None:
            vault.mkdir()
        else:
            shutil.copytree(args.scratch_vault, vault)
        if 'note' in scenario:
            (vault / 'scenario.md').write_text(scenario['note'])
        copy_vault_fixture(vault, scenario.get('vault'))
        write_fixtures(vault, scenario.get('generate', ()))
        for spec in scenario.get('generate_numbered', ()):
            write_fixtures(vault, ({'kind': 'text', 'path': path, 'text': spec['text'].format(i=i)}
                                   for i, path in enumerate(numbered_paths(spec))))
        rebase_jsonl_dates(vault, scenario.get('jsonl_latest_days_ago', {}))
        apply_mtimes(vault, scenario.get('mtime', {}), result)
        if scenario.get('vault_read_only'):
            # Like an archived vault after `chmod -R a-w`: every file and folder.
            for directory, _, files in os.walk(vault, topdown=False):
                for name in files:
                    (Path(directory) / name).chmod(0o444)
                Path(directory).chmod(0o555)
            require(not os.access(vault / '.okilum', os.W_OK) and not os.access(vault, os.W_OK),
                    'The read-only fixture vault is writable')
            result['vault_read_only'] = True
        for step in scenario['steps']:
            if 'text' in step:
                for name, mtime in result.get('vault_mtimes', {}).items():
                    step['text'] = step['text'].replace('{mtime_day:' + name + '}', mtime['jerusalem_day'])
        frames_dir = root / 'frames'
        frames_dir.mkdir()
        outside_file = root / 'outside' / 'External note.md'
        if 'outside_note' in scenario:
            outside_file.parent.mkdir()
            outside_file.write_text(scenario['outside_note'])
        companion = None
        if 'companion_vault' in scenario:
            spec = scenario['companion_vault']
            companion = root / spec.get('dir', 'companion')
            companion.mkdir()
            write_fixtures(companion, spec.get('generate', ()))
        state = root / 'home/state/okilum'
        state.mkdir(parents=True)
        if 'sidebar_state' in scenario:
            key = hashlib.sha256(os.fsencode(vault)).hexdigest()[:24]
            sidebar = state / 'sidebar'
            sidebar.mkdir()
            (sidebar / f'{key}.json').write_text(json.dumps(scenario['sidebar_state']))
        (state / 'reader-ui.json').write_text(json.dumps({
            'version': 1, 'appearance': scenario.get('appearance', 'light'),
            **scenario.get('preferences', {}),
        }))
        if 'features' in scenario:
            (state / 'features.json').write_text(json.dumps(scenario['features']))
        for spec in scenario.get('state_files', ()):
            rel = spec['path']
            require(not Path(rel).is_absolute() and '..' not in Path(rel).parts,
                    f'State fixture path must stay inside state: {rel}')
            target = state / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            if 'base64' in spec:
                target.write_bytes(base64.b64decode(spec['base64']))
            else:
                target.write_text(spec['text'], encoding='utf-8')
        rig_options = scenario.get('rig', {})
        require(set(rig_options) <= {'output_scale', 'floating', 'persistent_pointer'},
                f'Unknown rig keys: {rig_options}')
        rig = Rig(root, 1366, input_methods=None, **rig_options)
        rig.env['OKILUM_STATE_DIR'] = str(state)
        proc = None
        try:
            with (output / 'app.log').open('w') as log:
                def launch(note, *, restore=False, cutover=False):
                    argv = [binary, '--index-dir', str(root / ('index-grove' if cutover else 'index'))]
                    if not restore:
                        argv.extend(['--vault', str(vault)])
                    if note is not None:
                        argv.extend(['--note', note])
                    return subprocess.Popen(argv, env=rig.env, stdout=log, stderr=log)

                start = time.monotonic()
                proc = launch(scenario.get('open', 'scenario.md'))
                # The app's after-first-frame diagnostic, not a sleep/window-exists proxy.
                deadline = time.monotonic() + 45
                while time.monotonic() < deadline:
                    frame = [e for e in trace_events(root) if e.get('phase') == 'ui_scale'
                             and e.get('details', {}).get('reason') == 'first_frame']
                    if frame:
                        result['launch_to_first_frame_seconds'] = round(time.monotonic() - start, 3)
                        result['first_frame_event'] = frame[0]
                        break
                    if proc.poll() is not None:
                        raise RuntimeError(f'App exited before first frame: {proc.returncode}')
                    time.sleep(.05)
                else:
                    raise AssertionError('No first-frame diagnostic within 45s')
                # Ready means fixture content is actually readable in the note pane.
                shot = output / 'ready.png'
                deadline = time.monotonic() + 20
                while True:
                    rig.shot(shot)
                    visible = text(shot, scenario['region'])
                    if scenario['ready_text'] in visible:
                        break
                    if time.monotonic() > deadline:
                        raise AssertionError(f'Fixture content not visible: {visible!r}')
                if 'wait_for_vault' in scenario:
                    deadline = time.monotonic() + scenario['wait_for_vault']
                    while not any(e.get('phase') == 'vault_ready' for e in trace_events(root)):
                        require(proc.poll() is None, f'App exited while indexing: {proc.returncode}')
                        require(time.monotonic() < deadline, 'Vault indexing did not finish')
                        time.sleep(.25)
                result['checks'] = []
                for step in scenario['steps']:
                    name = step['id']
                    shot = output / f'{name}.png'
                    check = {'id': name, 'screenshot': str(shot)}
                    result['checks'].append(check)
                    with check_gate(step, check, result):
                        if 'max_ms' in step:
                            timed_step(rig, step, scenario, output, frames_dir, check)
                        else:
                            if 'open_url' in step:
                                url = step['open_url'].replace('{outside_file}', quote(str(outside_file)))
                                require(url.startswith('okilum://'), 'open_url must be an okilum:// URL')
                                command(binary, url, env=rig.env, stdout=log, stderr=log, timeout=15)
                            if 'open_args' in step:
                                argv = [
                                    a.replace('{vault}', str(vault))
                                     .replace('{companion}', str(companion or ''))
                                     .replace('{outside_file}', str(outside_file))
                                    for a in step['open_args']
                                ]
                                command(binary, *argv, env=rig.env, stdout=log, stderr=log, timeout=15)
                            if 'keys' in step:
                                rig.keys(*step['keys'], mods=step.get('mods', ()))
                            if 'click' in step:
                                rig.click(*step['click'])
                            if 'hover' in step:
                                rig.move_pointer(*step['hover'])
                            if 'type' in step:
                                # wtype interprets a standalone hyphen as stdin.
                                # Feed the whole literal query through a closed pipe.
                                command('wtype', '-', input=step['type'], text=True,
                                        env=rig.env, timeout=10)
                            if 'vault_replace' in step:
                                replace_vault_text(vault, step['vault_replace'])
                            if 'vault_create' in step:
                                create_vault_text(vault, step['vault_create'])
                            if step.get('restart'):
                                rig.keys('q', mods=['ctrl'])
                                require(proc.wait(timeout=15) == 0, 'App did not exit cleanly')
                                proc = None
                            if 'reopen' in step:
                                proc.terminate()
                                proc.wait(timeout=15)
                                proc = None
                            if 'vault_remove' in step:
                                remove_vault_file(vault, step['vault_remove'])
                            if 'vault_move' in step:
                                move_vault_fixture(vault, step['vault_move'])
                            if 'cutover' in step:
                                vault = cutover_fixture(vault, step['cutover'])
                            if step.get('restart') or 'reopen' in step:
                                rig.close()
                                rig = Rig(root, 1366, input_methods=None, **rig_options)
                                rig.env['OKILUM_STATE_DIR'] = str(state)
                                cutover = 'cutover' in step
                                proc = launch(step.get('reopen'),
                                              restore=cutover or step.get('restart') == 'session',
                                              cutover=cutover)
                            time.sleep(step.get('wait', .4))
                            rig.shot(shot)
                            if set(step) & {'vault_replace', 'vault_create', 'vault_remove', 'vault_move', 'cutover', 'restart'}:
                                deadline = time.monotonic() + 10
                                while time.monotonic() < deadline and not condition(shot, step, scenario['region'])[0]:
                                    require(proc.poll() is None, f'App exited: {proc.returncode}')
                                    time.sleep(.2)
                                    rig.shot(shot)
                        if (('max_ms' not in step and 'text' in step)
                                or set(step) & {'text_absent', 'text_order', 'declared_tree_roots'}):
                            check['text'] = text(shot, step.get('region', scenario['region']))
                            if 'max_ms' not in step and 'text' in step:
                                require(step['text'] in check['text'], check)
                            if 'text_absent' in step:
                                require(step['text_absent'] not in check['text'], check)
                            if 'text_order' in step:
                                pos = 0
                                for needle in step['text_order']:
                                    found = check['text'].find(needle, pos)
                                    require(found != -1, {**check, 'missing_in_order': needle})
                                    pos = found + len(needle)
                            if step.get('declared_tree_roots'):
                                check_declared_tree_roots(vault, check['text'], check)
                        if 'layout' in step:
                            check_layout(words(shot, step.get('region', scenario['region'])),
                                         step['layout'], check)
                        if 'max_ms' not in step and 'pixels' in step:
                            rule = step['pixels']
                            check['pixels'] = pixels(shot, rule['region'], rule['expression'])
                            require(rule['min'] <= check['pixels'] <= rule['max'], check)
                        if 'changed_from' in step:
                            before = output / f"{step['changed_from']}.png"
                            delta = command('convert', before, shot, '-compose', 'difference', '-composite',
                                            'png:-', capture_output=True).stdout
                            diff = output / f'{name}-diff.png'
                            diff.write_bytes(delta)
                            check['changed_pixels'] = pixels(diff, scenario['region'], 'max(r,max(g,b))>0.08?1:0')
                            require(check['changed_pixels'] >= step.get('min_changed_pixels', 100), check)
                            require(check['changed_pixels'] <= step.get('max_changed_pixels', float('inf')), check)
                        if 'window' in step:
                            # Compositor-side window box (logical px): `rect` is
                            # where sway places the window, `geometry` its natural
                            # size. Values within 1 px (fractional scale); with
                            # `from: <step id>` they are differences to that step.
                            rule = dict(step['window'])
                            field = rule.pop('field', 'rect')
                            base = rule.pop('from', None)
                            check['window'] = rig.window()
                            box = dict((check['window'] or {}).get(field) or {})
                            if base is not None:
                                earlier = next(c for c in result['checks'] if c['id'] == base)['window'][field]
                                box = {k: v - earlier[k] for k, v in box.items()}
                                check['window_delta'] = box
                            require(all(abs(box.get(k, float('nan')) - v) <= 1 for k, v in rule.items()), check)
                        if 'ui_scale' in step:
                            scales = [e['details']['scale']['ui_zoom'] for e in trace_events(root)
                                      if e.get('phase') == 'ui_scale']
                            check['ui_scale'] = scales[-1] if scales else None
                            require(check['ui_scale'] is not None
                                    and abs(check['ui_scale'] - step['ui_scale']) < 1e-6, check)
                result['passed'] = all(result.get('gates', {}).values())
        finally:
            if proc is not None:
                if not result.get('passed'):
                    try:
                        rig.shot(output / 'failure.png')
                    except (OSError, subprocess.SubprocessError):
                        pass
                if proc.poll() is None:
                    proc.terminate()
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()
            for path in (root / 'home').rglob('reader-diagnostic.log'):
                shutil.copy2(path, output / 'reader-diagnostic.log')
            rig.close()


def incremental_env():
    """Dev-loop builds are incremental (warm one-line rebuild ~17 s vs ~72 s).

    sccache refuses incremental, so it is turned off for these runs only.
    OKILUM_VERIFY_INCREMENTAL=0 restores the pool's non-incremental default.
    """
    if os.environ.get('OKILUM_VERIFY_INCREMENTAL', '1') == '0':
        return []
    return ['--env', 'CARGO_INCREMENTAL=1', '--env', 'BUILD_SLOT_SCCACHE=off']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('scenario', help='acceptance/<scenario>.yaml')
    parser.add_argument('--output', type=Path, help='Evidence directory (pool: under ~/work/qa-evidence)')
    parser.add_argument('--in-slot', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--binary', help='Use an already-built app for regression/bisect; skips build')
    parser.add_argument('--scratch-vault', type=Path,
                        help='Copy a rig-host scratch vault into the isolated run (requires --binary)')
    parser.add_argument('--measure-incremental', action='store_true', help='Time a one-line shell change in the slot')
    parser.add_argument('--live-cutover', action='store_true',
                        help='Check the root switch with the archive window still open')
    args = parser.parse_args()
    if args.scratch_vault is not None:
        args.scratch_vault = args.scratch_vault.resolve()
        if not args.binary or not args.scratch_vault.is_dir():
            parser.error('--scratch-vault requires --binary and an existing directory')
    if Path(args.scenario).name != args.scenario:
        parser.error('scenario must be an id, not a path')
    if not (ROOT / 'acceptance' / f'{args.scenario}.yaml').is_file():
        parser.error(f'Unknown scenario: {args.scenario}')
    if args.output is None:
        base = Path.home() / 'work/qa-evidence'
        base.mkdir(parents=True, exist_ok=True)
        args.output = Path(tempfile.mkdtemp(prefix=f"verify-{args.scenario}{'-live-cutover' if args.live_cutover else ''}-", dir=base))
    args.output = args.output.resolve()
    if not args.in_slot and not args.binary and not args.output.is_relative_to(Path.home() / 'work/qa-evidence'):
        parser.error('Pool evidence must be under ~/work/qa-evidence so the pool returns it')
    args.output.mkdir(parents=True, exist_ok=True)
    if not args.in_slot and not args.binary:
        # Stream the working tree too: acceptance can be written before its fix.
        revision = capture('git', '-C', ROOT, 'rev-parse', 'HEAD').strip()
        (args.output / 'source.json').write_text(json.dumps({
            'commit': revision,
            'status': capture('git', '-C', ROOT, 'status', '--short'),
        }, indent=2) + '\n')
        start = time.monotonic()
        cmd = ['okilum-slot', 'run', '--owner', 'acceptance-verify',
               '--label', f'verify-{args.scenario}', '--require-host',
               os.environ.get('OKILUM_VERIFY_HOST', 'okilum-dev'),
               '--env', f'OKILUM_SOURCE_COMMIT={revision}',
               '--mem', os.environ.get('OKILUM_VERIFY_MEM_GB', '6'),
               *incremental_env(),
               '--source-dir', str(ROOT), '--receipt', str(args.output / 'slot.json'),
               '--', 'python3', 'scripts/verify.py', args.scenario,
               '--in-slot', '--output', str(args.output),
               *(['--measure-incremental'] if args.measure_incremental else []),
               *(['--live-cutover'] if args.live_cutover else [])]
        env = dict(os.environ, OKILUM_SOURCE_COMMIT=revision)
        log_path = args.output / 'verify.log'
        if os.environ.get('OKILUM_VERIFY_RAW_OUTPUT') == '1':
            completed = subprocess.run(cmd, cwd=ROOT, env=env)
            wall_seconds = round(time.monotonic() - start, 3)
            (args.output / 'command.json').write_text(json.dumps({
                'wall_seconds': wall_seconds, 'exit_code': completed.returncode,
            }, indent=2) + '\n')
            print(f'Evidence: {args.output}', flush=True)
        else:
            with log_path.open('w', encoding='utf-8', errors='replace') as log_fh:
                completed = subprocess.run(cmd, cwd=ROOT, env=env, stdout=log_fh, stderr=subprocess.STDOUT)
            wall_seconds = round(time.monotonic() - start, 3)
            (args.output / 'command.json').write_text(json.dumps({
                'wall_seconds': wall_seconds, 'exit_code': completed.returncode,
            }, indent=2) + '\n')
            sys.stdout.write(format_verify_summary(
                args.scenario, completed.returncode, wall_seconds, args.output, log_path
            ))
            sys.stdout.flush()
        return 0 if completed.returncode == 0 else 1
    start = time.monotonic()
    result = {'scenario': args.scenario, 'host': socket.gethostname(), 'passed': False}
    try:
        import yaml
        scenario = yaml.safe_load((ROOT / 'acceptance' / f'{args.scenario}.yaml').read_text())
        result['source_commit'] = os.environ.get('OKILUM_SOURCE_COMMIT')
        if args.live_cutover:
            cutover = next((step for step in scenario['steps'] if 'cutover' in step), None)
            require(cutover is not None, 'The scenario must define a cutover step')
            cutover.pop('restart', None)
            cutover.pop('pixels', None)
            cutover.update(text='cutover', region=[0, 70, 1366, 560])
            scenario['steps'] = scenario['steps'][:scenario['steps'].index(cutover) + 1]
            result['live_cutover'] = True
        run(args, scenario, args.output, result)
    except Exception as error:
        result['error'] = f'{type(error).__name__}: {error}'
    result['verify_seconds'] = round(time.monotonic() - start, 3)
    (args.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2), flush=True)
    print(f'Evidence: {args.output}', flush=True)
    return 0 if result['passed'] else 1


if __name__ == '__main__':
    sys.exit(main())
