#!/usr/bin/env python3
"""Small data-driven Linux acceptance runner, using the existing Wayland rig."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from urllib.parse import quote

ROOT = Path(__file__).resolve().parent.parent


def command(*args, **kwargs):
    return subprocess.run(list(map(str, args)), check=True, **kwargs)


def capture(*args):
    return command(*args, capture_output=True, text=True).stdout


def require(condition, details):
    if not condition:
        raise AssertionError(details)


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


def write_fixtures(vault, specs):
    for spec in specs:
        target = vault / spec['path']
        require(not Path(spec['path']).is_absolute() and '..' not in Path(spec['path']).parts,
                f'Fixture path must stay inside the vault: {spec["path"]}')
        target.parent.mkdir(parents=True, exist_ok=True)
        kind = spec['kind']
        require(kind in ('markdown', 'json'), f'Unknown fixture kind: {kind}')
        target.write_bytes(generate_markdown(spec) if kind == 'markdown' else generate_json(spec))


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
        vault = root / 'vault'
        vault.mkdir()
        if 'note' in scenario:
            (vault / 'scenario.md').write_text(scenario['note'])
        write_fixtures(vault, scenario.get('generate', ()))
        frames_dir = root / 'frames'
        frames_dir.mkdir()
        outside_file = root / 'outside' / 'External note.md'
        if 'outside_note' in scenario:
            outside_file.parent.mkdir()
            outside_file.write_text(scenario['outside_note'])
        state = root / 'home/state/okilum'
        state.mkdir(parents=True)
        (state / 'reader-ui.json').write_text(json.dumps({
            'version': 1, 'appearance': scenario.get('appearance', 'light'),
            **scenario.get('preferences', {}),
        }))
        rig_options = scenario.get('rig', {})
        require(set(rig_options) <= {'output_scale', 'floating'}, f'Unknown rig keys: {rig_options}')
        rig = Rig(root, 1366, input_methods=None, **rig_options)
        rig.env['OKILUM_STATE_DIR'] = str(state)
        proc = None
        try:
            with (output / 'app.log').open('w') as log:
                start = time.monotonic()
                proc = subprocess.Popen([binary, '--vault', str(vault), '--index-dir', str(root / 'index'),
                                         '--note', scenario.get('open', 'scenario.md')], env=rig.env, stdout=log, stderr=log)
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
                result['checks'] = []
                for step in scenario['steps']:
                    name = step['id']
                    shot = output / f'{name}.png'
                    check = {'id': name, 'screenshot': str(shot)}
                    result['checks'].append(check)
                    if 'max_ms' in step:
                        timed_step(rig, step, scenario, output, frames_dir, check)
                    else:
                        if 'open_url' in step:
                            url = step['open_url'].replace('{outside_file}', quote(str(outside_file)))
                            require(url.startswith('okilum://'), 'open_url must be an okilum:// URL')
                            command(binary, url, env=rig.env, stdout=log, stderr=log, timeout=15)
                        if 'keys' in step:
                            rig.keys(*step['keys'], mods=step.get('mods', ()))
                        if 'click' in step:
                            rig.click(*step['click'])
                        time.sleep(step.get('wait', .4))
                        rig.shot(shot)
                    if 'max_ms' not in step and 'text' in step:
                        check['text'] = text(shot, step.get('region', scenario['region']))
                        require(step['text'] in check['text'], check)
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
                result['passed'] = True
        finally:
            if proc is not None:
                if not result.get('passed'):
                    try:
                        rig.shot(output / 'failure.png')
                    except (OSError, subprocess.SubprocessError):
                        pass
                proc.terminate()
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()
            for path in (root / 'home').rglob('reader-diagnostic.log'):
                shutil.copy2(path, output / 'reader-diagnostic.log')
            rig.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('scenario', help='acceptance/<scenario>.yaml')
    parser.add_argument('--output', type=Path, help='Evidence directory (pool: under ~/work/qa-evidence)')
    parser.add_argument('--in-slot', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--binary', help='Use an already-built app for regression/bisect; skips build')
    parser.add_argument('--measure-incremental', action='store_true', help='Time a one-line shell change in the slot')
    args = parser.parse_args()
    if Path(args.scenario).name != args.scenario:
        parser.error('scenario must be an id, not a path')
    if not (ROOT / 'acceptance' / f'{args.scenario}.yaml').is_file():
        parser.error(f'Unknown scenario: {args.scenario}')
    if args.output is None:
        base = Path.home() / 'work/qa-evidence'
        base.mkdir(parents=True, exist_ok=True)
        args.output = Path(tempfile.mkdtemp(prefix=f'verify-{args.scenario}-', dir=base))
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
        completed = subprocess.run(['okilum-slot', 'run', '--owner', 'acceptance-verify',
                               '--label', f'verify-{args.scenario}', '--require-host',
                               os.environ.get('OKILUM_VERIFY_HOST', 'okilum-dev'),
                               '--env', f'OKILUM_SOURCE_COMMIT={revision}',
                               '--source-dir', str(ROOT), '--receipt', str(args.output / 'slot.json'),
                               '--', 'python3', 'scripts/verify.py', args.scenario,
                               '--in-slot', '--output', str(args.output),
                               *(['--measure-incremental'] if args.measure_incremental else [])], cwd=ROOT,
                              env=dict(os.environ, OKILUM_SOURCE_COMMIT=revision))
        (args.output / 'command.json').write_text(json.dumps({
            'wall_seconds': round(time.monotonic() - start, 3), 'exit_code': completed.returncode,
        }, indent=2) + '\n')
        print(f'Evidence: {args.output}', flush=True)
        return 0 if completed.returncode == 0 else 1
    start = time.monotonic()
    result = {'scenario': args.scenario, 'host': socket.gethostname(), 'passed': False}
    try:
        import yaml
        scenario = yaml.safe_load((ROOT / 'acceptance' / f'{args.scenario}.yaml').read_text())
        result['source_commit'] = os.environ.get('OKILUM_SOURCE_COMMIT')
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
