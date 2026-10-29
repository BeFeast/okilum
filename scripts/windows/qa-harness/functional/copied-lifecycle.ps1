# Canonical copied-build lifecycle. Load inside qa desktop via run-evidence.sh.
function ReadCopiedContract([string]$Directory){
 if($Directory -cnotmatch '^C:\\Users\\Public\\okilum-qa\\functional-[a-f0-9]{32}$'){throw 'Foreign copied root'}
 $c=Get-Content (Join-Path $Directory 'admission.json') -Raw -Encoding UTF8|ConvertFrom-Json
 if($c.executable -cne 'current/okilum.exe'){throw 'Wrong copied launch path'}
 if($c.source -cnotmatch '^[a-f0-9]{40}$' -or $c.version -cnotmatch '^0\.1\.[0-9]+$'){throw 'Invalid copied identity'}
 if((Get-FileHash (Join-Path $Directory 'app.zip')).Hash.ToLowerInvariant() -cne $c.bundle_sha256){throw 'Original portable ZIP changed'}
 $c
}
function AssertCopiedFiles([string]$Directory,$Contract){
 $app=Join-Path $Directory 'app'
 if(!(Test-Path -LiteralPath $app -PathType Container)){throw 'Copied app missing'}
 foreach($property in $Contract.files.PSObject.Properties){
  $name=$property.Name
  if($name -match '(^/|\\|:|(^|/)\.\.(/|$))'){throw 'Unsafe copied manifest path'}
  $path=Join-Path $app $name
  if((Get-Item -LiteralPath $path -Force).Attributes -band [IO.FileAttributes]::ReparsePoint){throw 'Reparse copied file'}
  if((Get-FileHash -LiteralPath $path).Hash.ToLowerInvariant() -cne $property.Value){throw ('Copied file changed: '+$name)}
 }
 $actual=@(Get-ChildItem -LiteralPath $app -Recurse -Force -File)
 if($actual.Count -ne @($Contract.files.PSObject.Properties).Count){throw 'Unexpected copied payload file'}
 foreach($entry in @(Get-ChildItem -LiteralPath $app -Recurse -Force)){
  if($entry.Attributes -band [IO.FileAttributes]::ReparsePoint){throw 'Reparse copied payload'}
 }
}
function AssertCopiedProcess([string]$Directory,$Contract,$Identity){
 AssertCopiedFiles $Directory $Contract
 if($Identity.pid -le 0 -or !$Identity.start_utc){throw 'Missing owned process identity'}
 $all=@(Get-Process okilum -ErrorAction SilentlyContinue)
 if($all.Count -ne 1 -or $all[0].Id -ne $Identity.pid){throw 'Foreign/additional app; no action'}
 $p=Get-Process -Id $Identity.pid -ErrorAction Stop
 $exe=Join-Path $Directory 'app\current\okilum.exe'
 if($p.SessionId -ne 1 -or $p.MainModule.FileName -cne $exe -or $p.StartTime.ToUniversalTime().ToString('o') -cne $Identity.start_utc){throw 'Foreign/reused process identity'}
 $descriptor=Get-Content (Join-Path $Directory 'state\reader-instance.json') -Raw|ConvertFrom-Json
 if($descriptor.pid -ne $p.Id){throw 'Wrong single-instance owner'}
 $start=@(Get-Content (Join-Path $Directory 'state\reader-diagnostic.log')|ForEach-Object {try{$_|ConvertFrom-Json}catch{}}|Where-Object {$_.phase -eq 'process_start'})|Select-Object -Last 1
 $facts=Get-Content (Join-Path $Directory 'state\whats-new.json') -Raw|ConvertFrom-Json
 if(!$start -or $facts.source -cne $Contract.source -or [string]$facts.build -cne [string]$start.details.build -or $start.details.version -cne $Contract.version -or ($start.details.version_info -notlike ('*source '+$Contract.source.Substring(0,8)+'*') -or $start.details.version_info -notlike '*portable*')){throw 'Copied app actual source/version not proven'}
 $p
}
function SetCopiedEnvironment([string]$Directory){
 # State+index+instance endpoint are distinct from the installed application.
 $env:OKILUM_STATE_DIR=Join-Path $Directory 'state'
 $env:OKILUM_INDEX_DIR=Join-Path $Directory 'index'
}
function StartCopiedApp([string]$Directory,[string]$Vault){
 $contract=ReadCopiedContract $Directory
 AssertCopiedFiles $Directory $contract
 if(@(Get-Process okilum -ErrorAction SilentlyContinue).Count){throw 'Existing app; no installed/foreign fallback'}
 if($Vault -cnotmatch '^C:\\Users\\Public\\okilum-qa\\identity-[a-f0-9]{32}\\Identity(Seed|Failure|Renamed)$'){throw 'Foreign functional vault'}
 SetCopiedEnvironment $Directory
 $exe=Join-Path $Directory 'app\current\okilum.exe'
 $p=Start-Process -FilePath $exe -ArgumentList @('--vault',('"'+$Vault+'"'),'--note','Home.md') -PassThru
 $identity=[pscustomobject]@{pid=$p.Id;start_utc=$p.StartTime.ToUniversalTime().ToString('o');exe=$exe}
 $identity|ConvertTo-Json|Set-Content (Join-Path $Directory 'owned-process.json') -Encoding UTF8
 $all=@(Get-Process okilum -ErrorAction SilentlyContinue)
 if($all.Count -ne 1 -or $all[0].Id -ne $p.Id){throw 'Concurrent foreign app after launch; STOP'}
 $identity
}
function SendCopiedLink([string]$Directory,[string]$Uri){
 $contract=ReadCopiedContract $Directory
 $identity=Get-Content (Join-Path $Directory 'owned-process.json') -Raw|ConvertFrom-Json
 $owner=AssertCopiedProcess $Directory $contract $identity
 if($Uri -cnotmatch '^okilum://v/Identity(Seed|Failure|Renamed)/(Home|Landing)\.md(\?id=[A-Za-z0-9-]{8,64})?$'){throw 'Unknown scenario URI'}
 SetCopiedEnvironment $Directory
 $child=Start-Process -FilePath $identity.exe -ArgumentList $Uri -PassThru
 if(!$child.WaitForExit(15000)){throw 'Deep-link forwarding deadline; no second delivery'}
 if($child.ExitCode -ne 0){throw 'Deep-link forward process failed'}
 if($child.Id -eq $owner.Id){throw 'Invalid delivery process'}
 $null=AssertCopiedProcess $Directory $contract $identity
 # Delivery success alone does not prove navigation; scenario must observe it.
}
function CloseCopiedApp([string]$Directory){
 $contract=ReadCopiedContract $Directory
 $identity=Get-Content (Join-Path $Directory 'owned-process.json') -Raw|ConvertFrom-Json
 $p=AssertCopiedProcess $Directory $contract $identity
 $windows=@(Wins|Where-Object {$_.Pid -eq $p.Id -and $_.Proc -eq 'okilum'})
 if($windows.Count -ne 1 -or $windows[0].Class -cne 'Zed::Window' -or $windows[0].Title -notlike 'Okilum - *'){throw 'Exactly one owned Reader required for ordinary close'}
 Focus $windows[0]
 Keys '%{F4}'
 if(!$p.WaitForExit(20000)){throw 'Ordinary close did not exit; no force/reopen fallback'}
 if(@(Get-Process okilum -ErrorAction SilentlyContinue).Count){throw 'App remains after ordinary close'}
}
