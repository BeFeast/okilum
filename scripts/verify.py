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
        (vault / 'scenario.md').write_text(scenario['note'])
        state = root / 'home/state/okilum'
        state.mkdir(parents=True)
        (state / 'reader-ui.json').write_text(json.dumps({
            'version': 1, 'appearance': scenario.get('appearance', 'light'),
            **scenario.get('preferences', {}),
        }))
        rig = Rig(root, 1366, input_methods=None)
        rig.env['OKILUM_STATE_DIR'] = str(state)
        proc = None
        try:
            with (output / 'app.log').open('w') as log:
                start = time.monotonic()
                proc = subprocess.Popen([binary, '--vault', str(vault), '--index-dir', str(root / 'index'),
                                         '--note', 'scenario.md'], env=rig.env, stdout=log, stderr=log)
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
                    if 'keys' in step:
                        rig.keys(*step['keys'], mods=step.get('mods', ()))
                    if 'click' in step:
                        rig.click(*step['click'])
                    time.sleep(step.get('wait', .4))
                    shot = output / f'{name}.png'
                    rig.shot(shot)
                    check = {'id': name, 'screenshot': str(shot)}
                    result['checks'].append(check)
                    if 'text' in step:
                        check['text'] = text(shot, step.get('region', scenario['region']))
                        require(step['text'] in check['text'], check)
                    if 'pixels' in step:
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
