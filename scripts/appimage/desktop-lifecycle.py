#!/usr/bin/env python3
"""One source/hash-pinned ordinary-entrypoint phase; no fixture, compiler or feed."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import time

SOURCE = 'ef0dfc7ac066237127fa871953f7e9e731d5918d'
DIGEST = '6b03bdae9b94d5efb19e4b057c3834c68a72f1cdd7ba4975ad6da36539a5aa35'
ROOT = Path('/work/desktop-1716')
EVIDENCE = Path('/evidence/desktop')


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def wait(probe, label, seconds=60):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if probe():
            return
        time.sleep(.2)
    raise AssertionError('Deadline: ' + label)


def main():
    assert os.geteuid() != 0
    assert not ROOT.exists()
    EVIDENCE.mkdir(parents=True, exist_ok=False)
    facts = {'source': SOURCE, 'binary_sha256': DIGEST, 'harness_sha256': digest(Path(__file__)),
             'controls': [], 'result': 'BLOCKED', 'own_processes_after_cleanup': None}
    owned = []
    logs = []
    try:
        artifact = Path('/new/Okilum-x86_64.AppImage')
        provenance = json.loads(Path('/new/provenance.json').read_text())
        assert provenance['source_sha'] == SOURCE and provenance['sha256'] == DIGEST
        assert provenance['workflow_run'] == '40245818580'
        assert digest(artifact) == DIGEST
        facts['build'] = provenance['build_version']
        ROOT.mkdir(mode=0o700)
        environment = dict(os.environ)
        for name in ['APPIMAGE', 'APPDIR', 'LD_LIBRARY_PATH', 'APPIMAGE_EXTRACT_AND_RUN',
                     'OKILUM_APPIMAGE_UPDATE_RECEIPT', 'OKILUM_VAULT', 'OKILUM_INDEX_DIR']:
            environment.pop(name, None)
        for name, directory in {'HOME': 'home', 'XDG_CONFIG_HOME': 'config',
                                'XDG_CACHE_HOME': 'cache', 'XDG_DATA_HOME': 'data',
                                'XDG_STATE_HOME': 'xdg-state', 'XDG_RUNTIME_DIR': 'runtime',
                                'OKILUM_STATE_DIR': 'state', 'TMPDIR': 'tmp'}.items():
            path = ROOT / directory
            path.mkdir(mode=0o700)
            environment[name] = str(path)
        image = ROOT / 'first image.AppImage'
        shutil.copyfile(artifact, image)
        image.chmod(0o755)
        home = ROOT / 'home'
        desktop = home / '.local/share/applications/okilum-appimage.desktop'
        icon = home / '.local/share/icons/hicolor/scalable/apps/okilum-appimage.svg'
        peer = desktop.with_name('okilum.desktop')
        peer.parent.mkdir(parents=True)
        peer.write_bytes(b'distribution sentinel')
        note = ROOT / 'canonical-note.md'
        note.write_bytes(b'# Canonical note outside state\n')

        def intact():
            assert digest(image) == DIGEST, 'Executable identity changed'
            assert peer.read_bytes() == b'distribution sentinel'
            assert note.read_bytes() == b'# Canonical note outside state\n'

        def control(name):
            intact()
            facts['controls'].append(name)
            (EVIDENCE / 'receipt.json').write_text(json.dumps(facts, indent=2) + '\n')

        def cli(*args, stdin=None):
            result = subprocess.run([str(image), '--appimage-extract-and-run', *args],
                                    input=stdin, text=True, capture_output=True,
                                    env=environment, cwd=ROOT, timeout=90)
            with (EVIDENCE / 'cli.log').open('a') as log:
                log.write(repr(args) + '\n' + result.stdout + result.stderr + '\n')
            intact()
            return result

        for args in [('--help',), ('--build-info',)]:
            result = cli(*args)
            assert result.returncode == 0
            assert not desktop.exists() and not icon.exists()
            if args == ('--build-info',):
                assert SOURCE in result.stdout and facts['build'] in result.stdout
        control('help/build-info no desktop payload')
        probe = subprocess.Popen(['xmessage', '-name', 'desktop-positive-control', '-timeout', '10', 'control'],
                                 env=environment, start_new_session=True)
        owned.append(probe)
        wait(lambda: subprocess.run(['xdotool', 'search', '--onlyvisible', '--name',
                                     'desktop-positive-control'], capture_output=True).returncode == 0,
             'window detector positive control', 10)
        probe.terminate()
        probe.wait(timeout=10)
        control('visible-window detector positive control')

        def launch(label, foreign=False):
            log = (EVIDENCE / (label + '.log')).open('w')
            logs.append(log)
            process = subprocess.Popen([str(image), '--appimage-extract-and-run'], env=environment,
                                       cwd=ROOT, stdout=log, stderr=subprocess.STDOUT,
                                       start_new_session=True)
            owned.append(process)
            def visible():
                assert process.poll() is None, 'Application exited before chooser'
                result = subprocess.run(['xdotool', 'search', '--onlyvisible', '--name', '^Okilum$'],
                                        text=True, capture_output=True)
                return result.returncode == 0 and bool(result.stdout.strip())
            wait(visible, label + ' visible chooser')
            if not foreign:
                wait(lambda: desktop.exists() and icon.exists(), label + ' desktop payload')
                expected = f'Exec=/usr/bin/env -- "{image}" %u\n'
                assert expected in desktop.read_text(), desktop.read_text()
                approved = Path('/source/crates/okilum-shell/assets/brand/app-icon-light.svg').read_bytes()
                assert icon.read_bytes() == approved
                assert 'X-Okilum-AppImage=true\n' in desktop.read_text()
            control(label + ' ordinary chooser launch')
            return process

        def close(process):
            result = subprocess.run(['xdotool', 'search', '--onlyvisible', '--name', '^Okilum$'],
                                    capture_output=True, text=True, check=True)
            window = result.stdout.splitlines()[0]
            subprocess.run(['xdotool', 'windowfocus', '--sync', window], check=True, timeout=10)
            subprocess.run(['xdotool', 'key', '--clearmodifiers', 'ctrl+q'], check=True, timeout=10)
            assert process.wait(timeout=20) == 0
            intact()

        first = launch('first')
        result = cli('--uninstall-data', '--yes')
        assert result.returncode == 2 and 'Quit Okilum first' in result.stderr
        assert desktop.exists() and icon.exists()
        control('running-instance uninstall refused')
        close(first)
        original_entry = desktop.read_bytes()
        second = launch('relaunch')
        assert desktop.read_bytes() == original_entry
        close(second)
        control('idempotent relaunch')
        moved = ROOT / 'moved image.AppImage'
        image.rename(moved)
        image = moved
        third = launch('moved')
        assert desktop.read_bytes() != original_entry
        close(third)
        control('same-hash move refresh; not signed update')
        result = cli('--uninstall-data', stdin='n\n')
        assert result.returncode == 1 and 'Nothing was removed.' in result.stdout
        assert desktop.exists() and icon.exists()
        control('uninstall cancelled')
        result = cli('--uninstall-data', '--yes')
        assert result.returncode == 0
        for path in [desktop, icon]:
            assert 'Removed ' + str(path) in result.stdout
            assert not path.exists()
        control('confirmed uninstall exact owned-path report')
        desktop.write_bytes(b'foreign launcher')
        icon.write_bytes(b'foreign icon')
        fourth = launch('foreign', foreign=True)
        wait(lambda: 'Refusing to overwrite' in (EVIDENCE / 'foreign.log').read_text(),
             'foreign install refusal')
        assert desktop.read_bytes() == b'foreign launcher' and icon.read_bytes() == b'foreign icon'
        close(fourth)
        result = cli('--uninstall-data', '--yes')
        assert result.returncode == 1 and 'Refusing to remove' in result.stderr
        assert desktop.read_bytes() == b'foreign launcher' and icon.read_bytes() == b'foreign icon'
        control('foreign payloads and distribution/note sentinels preserved')
        facts['result'] = 'PASS'
    except BaseException as error:
        facts['failure'] = repr(error)
        raise
    finally:
        def group_members():
            groups = {p.pid for p in owned}
            members = []
            for entry in Path('/proc').iterdir():
                if entry.name.isdecimal():
                    try:
                        if os.getpgid(int(entry.name)) in groups:
                            members.append(int(entry.name))
                    except ProcessLookupError:
                        pass
            return members
        for process in owned:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            if process.poll() is None:
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
        # Read back all owned process groups, including extraction children.
        deadline = time.monotonic() + 5
        while group_members() and time.monotonic() < deadline:
            time.sleep(.1)
        for pid in group_members():
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        for log in logs:
            log.close()
        facts['own_processes_after_cleanup'] = group_members()
        if facts['own_processes_after_cleanup']:
            facts['result'] = 'BLOCKED'
        if ROOT.exists():
            shutil.rmtree(ROOT)
        facts['disposable_tree_absent'] = not ROOT.exists()
        (EVIDENCE / 'receipt.json').write_text(json.dumps(facts, indent=2) + '\n')


if __name__ == '__main__':
    main()
