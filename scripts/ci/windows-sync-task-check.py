#!/usr/bin/env python3
"""Exercise the production signed native Task/Connect/Stop factory on an empty hosted account."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

TEST = 'sidecar::windows::bootstrap::signed_task_tests::windows_signed_task_survives_package_move_and_owns_stop'


def diagnostics(state):
    """Read only the disposable fixture's public task status and private error."""
    system = Path(os.environ['SystemRoot']) / 'System32'
    commands = [[str(system / 'query.exe'), 'session'],
                [str(system / 'tasklist.exe'), '/FI', 'IMAGENAME eq okilum-sync-supervisor.exe', '/FO', 'CSV', '/NH']]
    journal = state / 'sidecar.json'
    if journal.exists():
        import uuid
        saved = json.loads(journal.read_text())
        instance = str(uuid.UUID(saved['binding']['instance']))
        commands.append([str(system / 'schtasks.exe'), '/Query', '/TN', 'Okilum-Sync-' + instance, '/V', '/FO', 'LIST'])
    for command in commands:
        result = subprocess.run(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, text=True, timeout=15)
        print(f'Native fixture diagnostic {Path(command[0]).name}: {result.stdout}', flush=True)
    failure = state / 'supervisor-error.log'
    if failure.exists():
        print('Native fixture helper failure: ' + failure.read_text()[:8192], flush=True)


def run(client, phase, evidence, state, env):
    selected = dict(env, OKILUM_NATIVE_TASK_FIXTURE_PHASE=phase,
                    OKILUM_NATIVE_TASK_FIXTURE_EVIDENCE=str(evidence),
                    OKILUM_NATIVE_TASK_FIXTURE_STATE=str(state))
    result = subprocess.run([str(client), TEST, '--exact', '--ignored', '--nocapture'], env=selected,
                            text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=240)
    print(result.stdout, flush=True)
    if result.returncode or 'test result: ok. 1 passed; 0 failed;' not in result.stdout:
        diagnostics(state)
        raise RuntimeError(f'signed native task {phase} failed ({result.returncode})')


def artifact(command, env, target):
    result = subprocess.run([*command, '--message-format=json'], env=env, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=600)
    files = []
    for line in result.stdout.splitlines():
        try:
            entry = json.loads(line)
        except ValueError:
            print(line, flush=True)
            continue
        if entry.get('reason') == 'compiler-message':
            print(entry['message'].get('rendered', entry['message']['message']), flush=True)
        if entry.get('reason') == 'compiler-artifact' and entry.get('executable') and entry['target']['name'] == target:
            files.append(Path(entry['executable']))
    if result.returncode or len(files) != 1:
        raise RuntimeError(f'native task artifact failed: {result.returncode}, {files}')
    return files[0]


def check(runtime, notices):
    if os.environ.get('GITHUB_ACTIONS') != 'true' or os.environ.get('RUNNER_OS') != 'Windows':
        raise RuntimeError('signed native task control requires a disposable hosted Windows account')
    state = Path(os.environ['LOCALAPPDATA']) / 'Okilum' / 'Sync'
    direct = state.with_name('SyncDirect')
    if state.exists() or direct.exists():
        raise RuntimeError('native task control refuses an existing account connection')
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'syncthing'))
    import windows_bundle
    powershell = ['pwsh.exe', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File',
                  str(Path(__file__).with_name('windows-sync-task-certificate.ps1').resolve())]
    with tempfile.TemporaryDirectory(prefix='okilum-native-task-') as temporary:
        root = Path(temporary).resolve()
        metadata = root / 'certificate.json'
        initial = root / 'installed-before-update'
        updated = root / 'installed-after-update'
        initial.mkdir()
        cleanup_client = None
        created = False
        try:
            subprocess.run([*powershell, '-Action', 'create', '-Metadata', str(metadata)], check=True, timeout=90, stdin=subprocess.DEVNULL)
            created = True
            certificate = json.loads(metadata.read_text(encoding='utf-8-sig'))
            env = dict(os.environ, CARGO_PROFILE_DEV_DEBUG='0', OKILUM_WINDOWS_SIGN_SUBJECT=certificate['subject'],
                       OKILUM_WINDOWS_SIGN_ISSUER_CHAIN=json.dumps(certificate['issuers']))
            helper = artifact(['cargo', 'build', '--locked', '-p', 'okilum-sync-supervisor'], env, 'okilum-sync-supervisor')
            client = artifact(['cargo', 'test', '--locked', '-p', 'okilum-sync-controller', '--lib', TEST, '--no-run'], env, 'okilum_sync_controller')
            shutil.copyfile(client, initial / 'okilum.exe')
            shutil.copyfile(helper, initial / 'okilum-sync-supervisor.exe')
            shutil.copyfile(runtime, initial / 'syncthing.exe')
            shutil.copytree(notices, initial / 'Syncthing')
            # Exercise the production signing input, including the pinned
            # upstream certificate removal required by the real ssign backend.
            windows_bundle.prepare_signing(initial)
            for executable in ['okilum.exe', 'okilum-sync-supervisor.exe', 'syncthing.exe']:
                subprocess.run([*powershell, '-Action', 'sign', '-Metadata', str(metadata), '-File', str(initial / executable)], check=True, timeout=90, stdin=subprocess.DEVNULL)
            evidence = root / 'native-task-evidence.json'
            cleanup_client = initial / 'okilum.exe'
            run(cleanup_client, 'initial', evidence, state, env)
            shutil.copytree(initial, updated)
            cleanup_client = updated / 'okilum.exe'
            # The actual service must run its retained helper/runtime, outside
            # the old package. Removing this source is the update positive control.
            shutil.rmtree(initial)
            run(cleanup_client, 'resumed', evidence, state, env)
            cleanup_client = None
            print('WINDOWS_SIGNED_TASK_UPDATE_PASS: signed production factory, retained pairing/identity, exact Job owner, captured Stop/exit and re-enable', flush=True)
        finally:
            if cleanup_client is not None and cleanup_client.exists():
                try:
                    run(cleanup_client, 'cleanup', root / 'native-task-evidence.json', state, env)
                except Exception as error:
                    print(f'Native fixture cleanup remains failed: {error}', flush=True)
            if created:
                subprocess.run([*powershell, '-Action', 'cleanup', '-Metadata', str(metadata)], check=True, timeout=90, stdin=subprocess.DEVNULL)


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('runtime', type=Path)
    parser.add_argument('notices', type=Path)
    args = parser.parse_args()
    check(args.runtime, args.notices)
