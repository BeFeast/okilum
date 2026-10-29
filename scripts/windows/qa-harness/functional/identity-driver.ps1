# Fixed #1414 scenario payload; canonical run-evidence desktop entry only.
$ErrorActionPreference='Stop'
. "$Q\functional\copied-lifecycle.ps1"
. "$Q\functional\copied-actions.ps1"
. "$Q\functional\metadata-fixture.ps1"
$config=Get-Content "$Q\functional\attempt.json" -Raw|ConvertFrom-Json
$directory=$config.directory;$fixture=$config.fixture
$contract=ReadCopiedContract $directory
if($fixture -cnotmatch '^C:\\Users\\Public\\okilum-qa\\identity-[a-f0-9]{32}$'){throw 'Invalid scenario fixture root'}
$evidence=Join-Path $directory 'evidence'
New-Item -ItemType Directory -Path $evidence -ErrorAction Stop|Out-Null
$steps=[ordered]@{}
function SaveIdentityBlob([byte[]]$Bytes){
 $name='blob-'+[Guid]::NewGuid().ToString('N')
 $path=Join-Path $evidence $name
 [IO.File]::WriteAllBytes($path,$Bytes)
 @{path=$name;sha256=(Get-FileHash $path).Hash.ToLowerInvariant()}
}
function SaveIdentityFrame($Frame){SaveIdentityBlob ([IO.File]::ReadAllBytes($Frame.file))}
function NewIdentityStep($Frame){
 $f=SaveIdentityFrame $Frame
 @{source=$contract.source;pid=$Frame.window.Pid;frame=$f;inspected_sha256=$null}
 # inspected_sha256 deliberately absent until independent host full-frame review.
}
function MetaBytes([string]$Root){[IO.File]::ReadAllBytes((Join-Path $Root '.okilum\vault.json'))}
function StartIdentityVault([string]$Root){
 $null=StartCopiedApp $directory $Root
 $clock=[Diagnostics.Stopwatch]::StartNew()
 do{
  try{$w=GetCopiedWindow $directory;if($w){Focus $w;return WaitCopiedNote $directory $Root 'Home.md'}}catch{if($clock.Elapsed.TotalSeconds -ge 90){throw}}
  Start-Sleep -Milliseconds 500
 }while($clock.Elapsed.TotalSeconds -lt 90)
 throw 'Copied initial identity readiness deadline'
}
$seed=Join-Path $fixture 'IdentitySeed';$failure=Join-Path $fixture 'IdentityFailure';$renamed=Join-Path $fixture 'IdentityRenamed'
NewIdentityFixture $seed;NewIdentityFixture $failure
$null=StartIdentityVault $seed
$before=SaveIdentityBlob (MetaBytes $seed)
$copy=CopyActualIdentityLink $directory 'first-copy'
$row=NewIdentityStep $copy.frame;$row.before=$before;$row.after=SaveIdentityBlob (MetaBytes $seed);$row.clipboard=$copy.clipboard
$steps['first-copy']=$row
$seedPid=$row.pid
CloseCopiedApp $directory
$null=StartIdentityVault $failure
$locked=$null
try{
 $locked=EnterIdentityWriteFailure (Join-Path $failure '.okilum\vault.json')
 $before=SaveIdentityBlob $locked.before
 $copy=CopyActualIdentityLink $directory 'write-failure'
 $row=NewIdentityStep $copy.frame;$row.before=$before;$row.after=SaveIdentityBlob (MetaBytes $failure);$row.clipboard=$copy.clipboard;$row.probe=$locked.probe
 $row.temporary_files_after=@(Get-ChildItem (Join-Path $failure '.okilum') -Force -File|Where-Object {$_.Name -ne 'vault.json'}|ForEach-Object {$_.Name})
 $steps['write-failure']=$row
}finally{if($locked){$locked.handle.Dispose()}}
CloseCopiedApp $directory
$null=StartIdentityVault $seed
$copy=CopyActualIdentityLink $directory 'reopen'
$row=NewIdentityStep $copy.frame;$row.previous_pid=$seedPid;$row.after=SaveIdentityBlob (MetaBytes $seed);$row.clipboard=$copy.clipboard
$steps['reopen']=$row
CloseCopiedApp $directory
Move-Item -LiteralPath $seed -Destination $renamed -ErrorAction Stop
$null=StartIdentityVault $renamed
$id=(Get-Content (Join-Path $renamed '.okilum\vault.json') -Raw|ConvertFrom-Json).id
foreach($name in @('valid-id','legacy')){
 $before=SaveIdentityBlob (MetaBytes $renamed)
 $uri=if($name -eq 'valid-id'){'okilum://v/IdentitySeed/Landing.md?id='+$id}else{'okilum://v/IdentityRenamed/Landing.md'}
 SendCopiedLink $directory $uri
 $frame=WaitCopiedNote $directory $renamed 'Landing.md'
 $row=NewIdentityStep $frame;$row.before=$before;$row.after=SaveIdentityBlob (MetaBytes $renamed);$row.uri=$uri
 $row.opened_note='Landing.md';$row.expected_note='Landing.md';$row.opened_root=$renamed;$row.expected_root=$renamed;$row.opened_folder='IdentityRenamed'
 if($name -eq 'valid-id'){$row.link_folder='IdentitySeed';$row.resolved_id=$id}
 $steps[$name]=$row
 SendCopiedLink $directory ('okilum://v/IdentityRenamed/Home.md?id='+$id)
 $null=WaitCopiedNote $directory $renamed 'Home.md'
}
$before=SaveIdentityBlob (MetaBytes $renamed)
$losing='losing-'+[Guid]::NewGuid().ToString('N')
$navigation=@{root=$renamed;note='Home.md'}
$uri='okilum://v/IdentitySeed/Landing.md?id='+$losing
SendCopiedLink $directory $uri
Start-Sleep -Seconds 2
$frame=SaveCopiedFrame $directory 'mismatch-refusal'
$row=NewIdentityStep $frame;$row.before=$before;$row.after=SaveIdentityBlob (MetaBytes $renamed);$row.uri=$uri;$row.losing_id=$losing;$row.link_folder='IdentitySeed'
# Actual root/note readiness plus owner-observed refusal, never a fabricated success.
$w=GetCopiedWindow $directory
if($w.Title -cne 'Okilum - Home - IdentityRenamed'){throw 'Mismatch navigated to another note/window'}
$row.navigation_before=$navigation;$row.navigation_after=@{root=$renamed;note='Home.md'};$row.visible_refusal=$null
$steps['mismatch']=$row
$receipt=[ordered]@{scenario='external-file-identity-v1';native_driver='external-file-identity-v1';mode='copied-build';source=$contract.source;steps_order=@('first-copy','write-failure','reopen','valid-id','legacy','mismatch');steps=$steps}
$receipt|ConvertTo-Json -Depth 12|Set-Content (Join-Path $evidence 'scenario-receipt.json') -Encoding UTF8
CloseCopiedApp $directory
Log 'IDENTITY SCENARIO EXECUTED; independent frame/refusal review REQUIRED; not native PASS'
