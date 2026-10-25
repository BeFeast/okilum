#!/usr/bin/env python3
"""Native Xvfb Settings/editor/restart gate. Only consumes unchanged PR artifacts.

The pair driver supplies a signed disposable fixture. No production trust or feeds
are changed. An old artifact without the native route is a hard gate blocker.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

IMAGE = 'Okilum-x86_64.AppImage'


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def wait_until(probe, label, timeout=90):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = probe()
        if result:
            return result
        time.sleep(0.2)
    raise AssertionError(f'Timed out: {label}; retained native workspace is evidence')


def main(old, new, fixture, fingerprint, evidence):
    assert os.geteuid() != 0
    evidence.mkdir(parents=True, exist_ok=False)
    for artifact in [old, new]:
        info = subprocess.run([artifact / IMAGE, '--appimage-extract-and-run', '--build-info'],
                              text=True, capture_output=True, timeout=90, check=True).stdout
        if 'appimage-gui-acceptance: yes\n' not in info:
            blocker = f'{artifact}: retained compiled artifact has no native GUI fixture route; do not relabel or rebuild it'
            (evidence / 'BLOCKED.txt').write_text(blocker + '\n')
            raise RuntimeError(blocker)
    old_p, new_p = [json.loads((a / 'provenance.json').read_bytes()) for a in [old, new]]
    assert int(old_p['build_version']) < int(new_p['build_version'])
    assert old_p['source_sha'] != new_p['source_sha']
    for artifact, facts in [(old, old_p), (new, new_p)]:
        assert facts['sha256'] == digest(artifact / IMAGE)
    work = evidence / 'work'; work.mkdir(mode=0o700)
    control = work / 'control'; control.mkdir()
    (control / 'fixture.json').write_text(json.dumps({'fixture': str(fixture), 'fingerprint': fingerprint}))
    target = work / 'Original with spaces.AppImage'
    shutil.copyfile(old / IMAGE, target); target.chmod(0o755)
    environment = dict(os.environ)
    for key in ['APPIMAGE', 'APPDIR', 'LD_LIBRARY_PATH', 'APPIMAGE_EXTRACT_AND_RUN', 'OKILUM_APPIMAGE_UPDATE_RECEIPT']:
        environment.pop(key, None)
    environment.update(HOME=str(work / 'home'), XDG_CONFIG_HOME=str(work / 'config'),
                       XDG_CACHE_HOME=str(work / 'cache'), OKILUM_STATE_DIR=str(work / 'state'),
                       XDG_RUNTIME_DIR=str(work / 'runtime'))
    for key in ['HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'OKILUM_STATE_DIR', 'XDG_RUNTIME_DIR']:
        Path(environment[key]).mkdir(mode=0o700)
    vaults = []
    for number in [1, 2]:
        vault = work / f'vault-{number}'; vault.mkdir()
        (vault / f'note-{number}.md').write_text(f'# Native window {number}\n\nOriginal canonical text.\n')
        vaults.append(vault)
    # X server/detector positive control before accepting absence/appearance.
    probe = subprocess.Popen(['xmessage', '-name', 'native-update-positive-control', '-timeout', '10', 'control'])
    def visible(name):
        return subprocess.run(['xdotool', 'search', '--onlyvisible', '--name', name], capture_output=True).returncode == 0
    try:
        wait_until(lambda: visible('native-update-positive-control'), 'X11 detector positive control', 10)
    finally:
        probe.terminate(); probe.wait(timeout=10)
    log = (evidence / 'native.log').open('w')
    process = subprocess.Popen([target, '--appimage-extract-and-run', '--appimage-gui-acceptance', control,
                                '--vault', vaults[0], '--note', 'note-1.md'], env=environment, stdout=log, stderr=log)
    serial = 0
    def command(action, **values):
        nonlocal serial
        serial += 1
        response = control / f'response-{serial}.json'
        tmp = control / 'command.tmp'
        tmp.write_text(json.dumps(dict(id=serial, action=action, **values)))
        tmp.replace(control / 'command.json')
        wait_until(response.is_file, f'native command {action}')
        result = json.loads(response.read_bytes())
        assert result['ok'], result
        return result['state']
    try:
        wait_until(lambda: visible('Okilum'), 'actual older native Reader')
        state = wait_until(lambda: (s if (s := command('snapshot'))['windows'] and all(w['ready'] for w in s['windows']) else None), 'older Reader loaded')
        assert state['identity']['source'] == old_p['source_sha']
        assert state['identity']['build'] == old_p['build_version']
        assert state['channel'] == 'Stable'
        command('settings')
        wait_until(lambda: visible('Okilum Settings'), 'native Settings')
        wait_until(lambda: command('snapshot')['settings_section'] == 'Updates', 'actual Settings Updates section')
        assert command('channel', beta=True)['channel'] == 'Beta'
        assert (work / 'state/appimage-update-channel').read_text() == 'beta'
        assert command('channel', beta=False)['channel'] == 'Stable'
        assert (work / 'state/appimage-update-channel').read_text() == 'stable'
        assert command('channel', beta=True)['channel'] == 'Beta'
        command('about')
        wait_until(lambda: command('snapshot')['settings_section'] == 'About', 'actual About section')
        command('settings')
        wait_until(lambda: command('snapshot')['settings_section'] == 'Updates', 'return to native Updates')
        command('open', vault=str(vaults[1]), note='note-2.md')
        before = wait_until(lambda: (s if len((s := command('snapshot'))['windows']) == 2 and all(w['ready'] for w in s['windows']) else None), 'two actual Readers')
        command('edit', text='first unsaved draft')
        wait_until(lambda: list((work / 'state/editor-drafts').glob('*.json')), 'real editor journal positive control')
        command('block-journal')
        latest = 'latest unsaved native acceptance draft'
        command('edit', text=latest)
        originals = {v: (v / f'note-{n}.md').read_bytes() for n, v in enumerate(vaults, 1)}
        command('stage')
        assert command('snapshot')['action'] == 'Restart to update'
        refused = command('activate')
        assert 'latest edits could not be protected' in refused['status'], refused
        assert any(w['editor'] == latest for w in refused['windows'])
        assert digest(target) == old_p['sha256']
        assert process.poll() is None and visible('Okilum Settings')
        assert all((v / f'note-{n}.md').read_bytes() == originals[v] for n, v in enumerate(vaults, 1))
        (evidence / 'refusal.json').write_text(json.dumps(refused, indent=2))
        command('repair-journal')
        command('activate')  # actual Settings action -> protect editors/state -> arm -> GPUI quit
        process.wait(timeout=150)
        assert process.returncode == 0
        stages = list(work.glob('.okilum-update-*')); assert len(stages) == 1
        stage = stages[0]
        wait_until(lambda: (stage / 'completed').is_file(), 'durable helper commit')
        assert (stage / 'completed').read_text() == 'committed\n'
        assert (stage / 'started').read_text() == 'startup confirmed\n'
        def helper_finished():
            if 'Startup confirmed; transaction committed and update lock released' not in (stage / 'apply.log').read_text():
                return False
            with (work / f'.{target.name}.update-lock').open('r+b') as lock:
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    return False
            return True
        wait_until(helper_finished, 'helper completion and lock release')
        # Acquire the actual lock while the restored GUI remains alive.
        with (work / f'.{target.name}.update-lock').open('r+b') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            assert digest(target) == new_p['sha256']
            assert digest(stage / 'previous.AppImage') == old_p['sha256']
        restored = wait_until(lambda: (s if len((s := command('snapshot'))['windows']) == 2 and all(w['ready'] for w in s['windows']) else None), 'actual multi-window restore')
        assert visible('Okilum')
        assert restored['identity']['build'] == new_p['build_version']
        assert restored['identity']['source'] == new_p['source_sha']
        assert restored['identity']['sha256'] == new_p['sha256']
        assert restored['channel'] == 'Beta'
        documents = lambda state: sorted((w['vault'], w['note']) for w in state['windows'])
        assert documents(restored) == documents(before)
        frames = lambda state: {(w['vault'], w['note']): w['bounds'] for w in state['windows']}
        assert frames(restored) == frames(before), 'native window bounds were not restored'
        assert any((v / f'note-{n}.md').read_text() == latest for n, v in enumerate(vaults, 1))
        restaged = command('restage-current-control')
        assert restaged['identity'] == restored['identity'] and visible('Okilum')
        assert digest(target) == new_p['sha256']
        (evidence / 'restaged-current-control.json').write_text(json.dumps(restaged, indent=2))
        (evidence / 'restored.json').write_text(json.dumps(restored, indent=2))
        (evidence / 'apply.log').write_bytes((stage / 'apply.log').read_bytes())
        command('quit')
        # The bounded harness also quits on its own after 300 seconds. Workspace
        # is deliberately retained on all paths, including failures/timeouts.
        report = dict(result='APPIMAGE_NATIVE_GUI_PASS', old=old_p, new=new_p,
                      before=before, refused=refused, restored=restored)
        (evidence / 'receipt.json').write_text(json.dumps(report, indent=2) + '\n')
        print(report['result'])
    finally:
        for applied in work.glob('.okilum-update-*/apply.log'):
            (evidence / 'apply.log').write_bytes(applied.read_bytes())
        log.close()
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=15)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['old', 'new', 'fixture', 'evidence']:
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--fingerprint', required=True)
    args = parser.parse_args()
    main(args.old.resolve(), args.new.resolve(), args.fixture.resolve(), args.fingerprint, args.evidence.resolve())
