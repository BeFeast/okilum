#!/usr/bin/env python3
"""ONE fixed copied-build scenario through canonical VM150 entry points."""
import datetime
import hashlib
import json
from pathlib import Path
import subprocess
import select
import sys
import time
import uuid
import zipfile
from copied_artifact import stage

HARNESS = Path('/home/god/.cache/okilum-qa/b1115d33/vm150')
ACTIONS = [(step, action) for step in ('first-copy', 'write-failure', 'reopen')
           for action in ('note-menu', 'copy-link')]


def execute(manifest, evidence, grant):
    if not grant.strip(): raise ValueError('Explicit attempt-specific grant required')
    evidence = Path(evidence).resolve()
    evidence.mkdir(exist_ok=False)
    admission = stage(manifest, evidence/'admitted')
    nonce = uuid.uuid4().hex
    remote = r'C:\Users\Public\okilum-qa\functional-' + nonce
    owner = 'windows-1808-' + nonce
    start = time.time(); cutoff = start + 2700; release = start + 3600
    record = dict(source=admission['source'], owner=owner, grant=grant,
                  work_cutoff=cutoff, release_deadline=release, attempt='ONE',
                  install=False, scenario='external-file-identity-v1')
    (evidence/'attempt.json').write_text(json.dumps(record, indent=2)+'\n')
    for p in [Path('/home/god/.cache/okilum-qa/vm150.lock.d')]:
        if p.exists(): raise RuntimeError('Busy VM: no lock wait or takeover')
    advisory = Path('/home/god/.cache/okilum-qa/vm150.session.lock')
    if advisory.exists() and advisory.read_text().strip():
        raise RuntimeError('Advisory reservation present: STOP')
    own = False; active = None; quiet = False; result = 'BLOCKED'; calls = 0

    def answer(seconds):
        remaining=min(seconds,cutoff-time.time())
        if remaining<=0 or not select.select([sys.stdin],[],[],remaining)[0]:
            raise RuntimeError('Owner data deadline; no action permitted')
        text=sys.stdin.readline()
        if not text: raise RuntimeError('Owner input closed')
        return text

    def call(name, args, seconds=60):
        nonlocal calls
        calls += 1
        remaining = (release if name.startswith('cleanup') else cutoff)-time.time()
        if remaining <= 5: raise RuntimeError('Original deadline exhausted')
        with (evidence/f'{calls:03d}-{name}.log').open('w') as log:
            completed = subprocess.run(['timeout', '--kill-after=5', str(int(min(seconds,remaining-5)))] +
                [str(x) for x in args], stdout=log, stderr=subprocess.STDOUT)
        with (evidence/'operations.jsonl').open('a') as log:
            log.write(json.dumps(dict(name=name, exit_code=completed.returncode,
                utc=datetime.datetime.now(datetime.timezone.utc).isoformat()))+'\n')
        if completed.returncode: raise RuntimeError(f'{name} terminal exit {completed.returncode}')
        return (evidence/f'{calls:03d}-{name}.log').read_text()

    def ps(name, body, seconds=60):
        return call(name, [HARNESS/'vm.sh','ps',body], seconds)

    def upload(name, local, target):
        call(name,[HARNESS/'vm.sh','put',local,target])

    def desktop(name, payload, seconds):
        return call(name,[HARNESS/'run-evidence.sh',payload,evidence,seconds],seconds+30)

    try:
        call('resource',[HARNESS/'vm.sh','qm150','qm status 150 && qm listsnapshot 150'])
        print(json.dumps(record),flush=True)
        print('Announce this exact owner/slot to current UI; read current reservations; then enter FREE.',flush=True)
        if answer(60).strip() != 'FREE': raise RuntimeError('Resource confirmation missing')
        # lock can fail after taking ownership; finally inspects its actual owner.
        call('lock',[HARNESS/'vm.sh','lock',owner],150)
        own = True
        source = Path(__file__).resolve().parent
        with zipfile.ZipFile(evidence/'helpers.zip','x',zipfile.ZIP_DEFLATED) as bundle:
            for name in ('copied-lifecycle.ps1','copied-actions.ps1','identity-driver.ps1'):
                bundle.write(source/name,'functional/'+name)
            bundle.write(source.parent/'scenarios/metadata-fixture.ps1','functional/metadata-fixture.ps1')
            bundle.write(HARNESS/'autosave-gate/session-quiet.ps1','session-quiet.ps1')
            for original,target in [(HARNESS/'step-envelope.ps1','controls/step-envelope.ps1'),
                                    (HARNESS/'autosave-gate/child-control.ps1','controls/child-control.ps1')]:
                bundle.write(original,target)
            bundle.writestr('controls/success/payload.ps1',"function Log($m){}; 'SUCCESS PAYLOAD'")
            bundle.writestr('controls/nested/payload.ps1',"function FailInner { throw [InvalidOperationException]::new('outer-message',[Exception]::new('inner-message')) }; FailInner")
        upload('helpers',evidence/'helpers.zip','C:/Users/Public/okilum-qa/helpers.zip')
        digest=hashlib.sha256((evidence/'helpers.zip').read_bytes()).hexdigest()
        ps('expand-helpers',f"$ErrorActionPreference='Stop'; $q='C:/Users/Public/okilum-qa'; if((Get-FileHash \"$q/helpers.zip\").Hash.ToLowerInvariant() -cne '{digest}'){{throw 'Helper mismatch'}}; Expand-Archive \"$q/helpers.zip\" $q; New-Item -ItemType Directory '{remote}'|Out-Null; 'STAGED';exit 0")
        upload('bundle',evidence/'admitted/app.zip',remote+'/app.zip')
        upload('admission',evidence/'admitted/admission.json',remote+'/admission.json')
        config=evidence/'config.json';config.write_text(json.dumps(dict(directory=remote.replace('/','\\'),fixture=('C:/Users/Public/okilum-qa/identity-'+nonce).replace('/','\\'))))
        upload('config',config,'C:/Users/Public/okilum-qa/functional/attempt.json')
        ps('verify-bundle',f"$ErrorActionPreference='Stop'; . 'C:/Users/Public/okilum-qa/functional/copied-lifecycle.ps1'; $c=ReadCopiedContract '{remote}'; Expand-Archive '{remote}/app.zip' '{remote}/app'; AssertCopiedFiles '{remote}' $c; 'ADMITTED';exit 0")
        gate=(HARNESS/'autosave-gate/gate-run.sh').read_text()
        first=gate.split('cat > "$E/first-envelope-control.ps1" <<\'PS\'\n',1)[1].split('\nPS\n',1)[0]
        control=evidence/'first-control.ps1';control.write_text(first+'\n')
        desktop('first-control',control,330)
        preparation=evidence/'quiet.ps1';preparation.write_text("$ErrorActionPreference='Stop'; if(@(Get-Process okilum -EA SilentlyContinue).Count){throw 'Unexpected app'}; . \"$Q/session-quiet.ps1\"; Log (EnterQuietSession $Q)\n")
        quiet=True;desktop('quiet',preparation,180)
        scenario=evidence/'scenario.ps1';scenario.write_text('. "$Q/functional/identity-driver.ps1"\n')
        active_log=(evidence/'scenario-controller.log').open('w')
        active=subprocess.Popen(['timeout','--kill-after=5','1830',str(HARNESS/'run-evidence.sh'),str(scenario),str(evidence),'1800'],stdout=active_log,stderr=subprocess.STDOUT)
        for step,action in ACTIONS:
            name=step+'-'+action; deadline=min(cutoff,time.time()+300)
            while time.time()<deadline:
                if active.poll() is not None: raise RuntimeError('Scenario ended before required action')
                text=ps('observe-'+name,f"$ErrorActionPreference='Stop'; if(Test-Path '{remote}/{name}-request.json'){{'READY'}}else{{'WAITING'}};exit 0")
                if text.strip()=='READY': break
                time.sleep(1)
            else: raise RuntimeError('Action observation deadline')
            request=evidence/(name+'-request.json')
            call('request-'+name,[HARNESS/'vm.sh','get',remote+'/'+name+'-request.json',request])
            r=json.loads(request.read_text(encoding='utf-8-sig'))
            frame=evidence/(name+'-inspected.png')
            call('frame-'+name,[HARNESS/'vm.sh','get',r['frame'].replace('\\','/'),frame])
            if hashlib.sha256(frame.read_bytes()).hexdigest()!=r['sha256']: raise RuntimeError('Frame hash changed')
            print(f'INSPECT {frame}; request {request}; provide JSON inspected_sha256,stage,action,label,x,y,width,height',flush=True)
            approval_data=json.loads(answer(70))
            if approval_data['inspected_sha256']!=r['sha256'] or approval_data['stage']!=step or approval_data['action']!=action: raise RuntimeError('Wrong action data')
            approval=evidence/(name+'-approval.json');approval.write_text(json.dumps(approval_data))
            upload('approval-'+name,approval,remote+'/'+name+'-approval.json.incoming')
            ps('publish-'+name,f"$ErrorActionPreference='Stop';[IO.File]::Move('{remote}/{name}-approval.json.incoming','{remote}/{name}-approval.json');exit 0")
        remaining=min(600,cutoff-time.time())
        if remaining<=0: raise RuntimeError('Scenario cutoff')
        if active.wait(timeout=remaining): raise RuntimeError('Scenario terminal failure')
        result='EXECUTED_PENDING_FRAME_REVIEW'
    finally:
        # No guest cleanup/rollback without proving this exact canonical owner.
        lock=Path('/home/god/.cache/okilum-qa/vm150.lock.d')
        own=lock.exists() and (lock/'owner').read_text().split()[0]==owner
        if own:
            # Stop only the owned task/app. A foreign or reused process retains lock.
            ps('cleanup-stop',f"$ErrorActionPreference='Stop'; $task=Get-ScheduledTask okilumqa-run -EA SilentlyContinue; if($task.State -eq 'Running'){{Stop-ScheduledTask okilumqa-run}}; $record='{remote}/owned-process.json'; $apps=@(Get-Process okilum -EA SilentlyContinue); foreach($p in $apps){{if(!(Test-Path $record)){{throw 'Unattributed app'}};$id=Get-Content $record -Raw|ConvertFrom-Json;if($p.Id -ne $id.pid -or $p.SessionId -ne 1 -or $p.MainModule.FileName -cne $id.exe -or $p.StartTime.ToUniversalTime().ToString('o') -cne $id.start_utc){{throw 'Foreign app'}}}}; foreach($p in $apps){{Stop-Process -Id $p.Id}}; 'APP0';exit 0")
            if active is not None and active.poll() is None:
                active.wait(timeout=45)
            if quiet:
                restore=evidence/'restore.ps1';restore.write_text('. "$Q/session-quiet.ps1";Log (ExitQuietSession $Q)\n')
                desktop('cleanup-restore',restore,180)
            ps('cleanup-archive',f"$ErrorActionPreference='Stop';$paths=@('{remote}','C:/Users/Public/okilum-qa/identity-{nonce}','C:/Users/Public/okilum-qa/controls','C:/Users/Public/okilum-qa/run');Get-ChildItem 'C:/Users/Public/okilum-qa' -Filter '*quiet*.json'|ForEach-Object {{$paths+=$_.FullName}};Compress-Archive -LiteralPath $paths -DestinationPath 'C:/Users/Public/okilum-qa/functional-raw.zip';exit 0",90)
            call('cleanup-download',[HARNESS/'vm.sh','get','C:/Users/Public/okilum-qa/functional-raw.zip',evidence/'raw.zip'],90)
            call('cleanup-rollback',[HARNESS/'vm.sh','unlock',owner],150)
            call('cleanup-observer',['python3',HARNESS/'autosave-gate/boot_readback.py',HARNESS/'vm.sh',evidence,int(release)],365)
        (evidence/'outcome.json').write_text(json.dumps(dict(status=result,lock_retained=lock.exists(),native_acceptance=False),indent=2)+'\n')
    return result


if __name__=='__main__':
    if len(sys.argv)!=5 or sys.argv[1]!='--execute':
        raise SystemExit('Usage: run-functional.py --execute MANIFEST NEW_EVIDENCE GRANT_TEXT')
    print(execute(sys.argv[2],sys.argv[3],sys.argv[4]))
