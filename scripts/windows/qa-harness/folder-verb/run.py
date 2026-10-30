#!/usr/bin/env python3
"""Canonical ONE installed-PR Explorer verb/install/uninstall scenario (#1914)."""
from pathlib import Path
import datetime
import hashlib
import io
import importlib.util
import json
import select
import subprocess
import sys
import time
import uuid
import zipfile

H = Path('/home/god/.cache/okilum-qa/b1115d33/vm150')


def admit(manifest):
    spec = importlib.util.spec_from_file_location('folder_verb_pr_seed', Path(__file__).with_name('pr_seed.py'))
    module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
    seed = module.load_pr_seed(Path(manifest))
    inputs = json.loads(Path(manifest).read_text())
    with zipfile.ZipFile(inputs['artifact_zip']) as archive:
        package = archive.read(f"BeFeast.Okilum-{seed['baseline']['package']}-beta-full.nupkg")
    with zipfile.ZipFile(io.BytesIO(package)) as archive:
        executable = archive.read('lib/app/okilum.exe')
    return dict(source=seed['source'], version=seed['baseline']['package'],
                exe_sha256=hashlib.sha256(executable).hexdigest(),
                setup_sha256=seed['baseline']['setup_sha256'], setup=seed['setup'])


def execute(manifest, directory, grant):
    if Path(__file__).resolve() != H / 'folder-verb/run.py':
        raise RuntimeError('Deploy reviewed files to canonical harness before native execution')
    if not grant.strip():
        raise ValueError('Explicit Manager native grant required')
    if subprocess.check_output(['git', '-C', str(H), 'status', '--porcelain'], text=True).strip():
        raise RuntimeError('Canonical checkout is dirty')
    e = Path(directory).resolve(); e.mkdir(exist_ok=False)
    admission = admit(manifest)
    nonce = uuid.uuid4().hex; owner = 'windows-1914-' + nonce
    remote = 'C:/Users/Public/okilum-qa/folder-verb-' + nonce
    started = time.time(); cutoff = started + 2700; release = started + 3600
    (e / 'attempt.json').write_text(json.dumps(dict(owner=owner, source=admission['source'],
        grant=grant, cutoff=cutoff, release=release, signing=False, promotion=False), indent=2))
    (e / 'head.txt').write_text(subprocess.check_output(['git', '-C', str(H), 'rev-parse', 'HEAD'], text=True))
    lock = Path('/home/god/.cache/okilum-qa/vm150.lock.d')
    advisory = Path('/home/god/.cache/okilum-qa/vm150.session.lock')
    if lock.exists() or (advisory.exists() and advisory.read_text().strip()):
        raise RuntimeError('Busy/reserved VM; no acquisition')
    import fcntl
    gui = advisory.open('a+'); fcntl.flock(gui, fcntl.LOCK_EX | fcntl.LOCK_NB)
    quiet = False; cleanup_errors = []; result = 'BLOCKED'; failure = None

    def call(name, args, seconds=60):
        budget = (release if name.startswith('cleanup') else cutoff) - time.time() - 5
        if budget <= 0:
            raise RuntimeError('Original attempt deadline exhausted')
        with (e / (name + '.log')).open('w') as log:
            r = subprocess.run(['timeout', '--kill-after=5', str(int(min(seconds, budget))),
                                *map(str, args)], stdout=log, stderr=subprocess.STDOUT)
        if len(args) > 1 and args[1] == 'ps' and (H / 'last-ps.stderr').exists():
            (e / (name + '.remote-stderr')).write_bytes((H / 'last-ps.stderr').read_bytes())
        with (e / 'operations.jsonl').open('a') as log:
            log.write(json.dumps(dict(name=name, exit=r.returncode, utc=datetime.datetime.now(datetime.timezone.utc).isoformat()))+'\n')
        print(name, r.returncode, flush=True)
        if r.returncode:
            raise RuntimeError(f'{name} terminal exit {r.returncode}')
        return (e / (name + '.log')).read_text()

    def ps(name, body, seconds=60):
        return call(name, [H / 'vm.sh', 'ps', body], seconds)

    def desktop(name, script, seconds):
        return call(name, [H / 'run-evidence.sh', script, e, seconds], seconds + 30)

    try:
        call('preflight', [H / 'vm.sh', 'qm150', 'qm status 150 && qm listsnapshot 150'])
        print(json.dumps(dict(owner=owner, admission=admission, cutoff=cutoff, release=release)), flush=True)
        print('Announce owner/slot to UI; check current reservations; enter FREE.', flush=True)
        if not select.select([sys.stdin], [], [], 60)[0] or sys.stdin.readline().strip() != 'FREE':
            raise RuntimeError('No resource confirmation')
        call('lock', [H / 'vm.sh', 'lock', owner], 180)
        (lock / 'expiry').write_text(datetime.datetime.fromtimestamp(release, datetime.timezone.utc).isoformat())
        (lock / 'thread').write_text('mcp:launch:177d4ea4cbc93a1b53df9af3593b6356c3fa5c06f6f4ce2aebb7484adab2f26a')
        with zipfile.ZipFile(e / 'helpers.zip', 'x', zipfile.ZIP_DEFLATED) as archive:
            archive.write(Path(__file__).with_name('driver.ps1'), 'folder-verb/driver.ps1')
            archive.write(H / 'autosave-gate/session-quiet.ps1', 'session-quiet.ps1')
            archive.write(H / 'step-envelope.ps1', 'controls/step-envelope.ps1')
            archive.write(H / 'autosave-gate/child-control.ps1', 'controls/child-control.ps1')
            archive.writestr('controls/success/payload.ps1', "function Log($m){}; 'SUCCESS PAYLOAD'")
            archive.writestr('controls/nested/payload.ps1', "function FailInner {throw [InvalidOperationException]::new('outer-message',[Exception]::new('inner-message'))};FailInner")
        call('upload-helpers', [H / 'vm.sh', 'put', e / 'helpers.zip', 'C:/Users/Public/okilum-qa/helpers.zip'])
        digest = hashlib.sha256((e / 'helpers.zip').read_bytes()).hexdigest()
        ps('stage', f"$ErrorActionPreference='Stop';$q='C:/Users/Public/okilum-qa';if((Get-FileHash \"$q/helpers.zip\").Hash.ToLowerInvariant() -cne '{digest}'){{throw 'Helper digest mismatch'}};Expand-Archive \"$q/helpers.zip\" $q;exit 0")
        admission['nonce'] = nonce
        (e / 'admission.json').write_text(json.dumps(admission, indent=2))
        call('upload-admission', [H / 'vm.sh', 'put', e / 'admission.json', 'C:/Users/Public/okilum-qa/folder-verb/attempt.json'])
        call('upload-setup', [H / 'vm.sh', 'put', admission['setup'], 'C:/Users/Public/okilum-qa/folder-verb/Setup.exe'])
        gate = (H / 'autosave-gate/gate-run.sh').read_text()
        control = gate.split('cat > "$E/first-envelope-control.ps1" <<\'PS\'\n', 1)[1].split('\nPS\n', 1)[0]
        (e / 'control.ps1').write_text(control)
        desktop('controls', e / 'control.ps1', 330)
        quiet = True
        ps('machine-quiet', '. "C:/Users/Public/okilum-qa/session-quiet.ps1";PrepareQuietMachine "C:/Users/Public/okilum-qa";exit 0', 90)
        (e / 'quiet.ps1').write_text('. "$Q/session-quiet.ps1";Log (EnterQuietSession $Q)\n')
        desktop('session-quiet', e / 'quiet.ps1', 300)
        (e / 'scenario.ps1').write_text('. "$Q/folder-verb/driver.ps1"\n')
        desktop('scenario', e / 'scenario.ps1', 1200)
        result = 'EXECUTED_PENDING_FRAME_REVIEW'
    except Exception as error:
        failure = str(error)
        (e / 'failure.txt').write_text(failure)
        print('STOP', failure, flush=True)
    finally:
        own = lock.exists() and (lock / 'owner').read_text().split()[0] == owner
        if own:
            def cleanup(name, action):
                try:
                    action()
                except Exception as error:
                    cleanup_errors.append(dict(step=name, error=str(error)))
            cleanup('stop', lambda: ps('cleanup-stop', "$ErrorActionPreference='Stop';$task=Get-ScheduledTask okilumqa-run -EA SilentlyContinue;if($task.State -eq 'Running'){Stop-ScheduledTask okilumqa-run};$root=Join-Path $env:LOCALAPPDATA 'BeFeast.Okilum';foreach($p in @(Get-Process okilum,okilum-sync-supervisor -EA SilentlyContinue)){if($p.SessionId -ne 1 -or !$p.MainModule.FileName.StartsWith($root+'\\',[StringComparison]::OrdinalIgnoreCase)){throw 'Foreign process; retain lock'};$start=$p.StartTime;$live=Get-Process -Id $p.Id -EA SilentlyContinue;if($live -and $live.StartTime -ne $start){throw 'Reused PID'};if($live){Stop-Process -InputObject $live}};exit 0"))
            if not any(error['step'] == 'stop' for error in cleanup_errors):
                if quiet:
                    cleanup('policy', lambda: ps('cleanup-policy', '. "C:/Users/Public/okilum-qa/session-quiet.ps1";RestoreQuietMachinePolicy "C:/Users/Public/okilum-qa";exit 0', 90))
                    (e / 'restore.ps1').write_text('. "$Q/session-quiet.ps1";Log (ExitQuietSession $Q)\n')
                    cleanup('session', lambda: desktop('cleanup-session', e / 'restore.ps1', 240))
                body = f"$ErrorActionPreference='Stop';$paths=@('{remote}','C:/Users/Public/okilum-qa/run','C:/Users/Public/okilum-qa/controls','C:/Users/Public/okilum-qa/folder-verb/attempt.json');$paths+=@(Get-ChildItem 'C:/Users/Public/okilum-qa' -Filter 'session-*.json'|ForEach-Object {{$_.FullName}});$missing=@($paths|Where-Object {{!(Test-Path -LiteralPath $_)}});ConvertTo-Json -InputObject $missing|Set-Content 'C:/Users/Public/okilum-qa/collection-missing.json';$paths=@($paths|Where-Object {{Test-Path -LiteralPath $_}});$paths+='C:/Users/Public/okilum-qa/collection-missing.json';Compress-Archive -LiteralPath $paths -DestinationPath 'C:/Users/Public/okilum-qa/folder-verb-raw.zip';exit 0"
                cleanup('archive', lambda: ps('cleanup-archive', body, 90))
                cleanup('download', lambda: call('cleanup-download', [H / 'vm.sh', 'get', 'C:/Users/Public/okilum-qa/folder-verb-raw.zip', e / 'raw.zip'], 90))
                # Unknown process ownership prevents further guest action or rollback.
                if not any(error['step'] == 'stop' for error in cleanup_errors):
                    cleanup('rollback', lambda: call('cleanup-rollback', [H / 'vm.sh', 'unlock', owner], 180))
                    cleanup('observer', lambda: call('cleanup-observer', ['python3', H / 'autosave-gate/boot_readback.py', H / 'vm.sh', e, int(release)], 365))
        (e / 'outcome.json').write_text(json.dumps(dict(status=result, failure=failure,
            cleanup_errors=cleanup_errors, lock_retained=lock.exists(), native_acceptance=False), indent=2))
        gui.close()
    return result


if __name__ == '__main__':
    if len(sys.argv) != 5 or sys.argv[1] != '--execute':
        raise SystemExit('Usage: run.py --execute MANIFEST NEW_EVIDENCE MANAGER_GRANT')
    print(execute(*sys.argv[2:]))
