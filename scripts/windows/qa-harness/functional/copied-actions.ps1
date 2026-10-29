# Fixed menu actions only. No caller script, keyboard macro or unreviewed click.
function GetCopiedWindow([string]$Directory){
 $c=ReadCopiedContract $Directory
 $id=Get-Content (Join-Path $Directory 'owned-process.json') -Raw|ConvertFrom-Json
 $p=AssertCopiedProcess $Directory $c $id
 $windows=@(Wins|Where-Object {$_.Pid -eq $p.Id -and $_.Class -ceq 'Zed::Window' -and $_.Title -like 'Okilum - *'})
 if($windows.Count -ne 1){throw 'Exactly one copied Reader window required'}
 $windows[0]
}
function SaveCopiedFrame([string]$Directory,[string]$Name){
 $w=GetCopiedWindow $Directory
 if([W]::GetForegroundWindow().ToInt64() -ne [long]$w.H){throw 'Copied window lost foreground'}
 Shot $Name $w
 $file=Join-Path $T ($Name+'.png')
 [pscustomobject]@{window=$w;file=$file;sha256=(Get-FileHash $file).Hash.ToLowerInvariant()}
}
function InvokeCopiedMenuAction([string]$Directory,[string]$Stage,[string]$Action){
 if($Stage -cnotin @('first-copy','write-failure','reopen') -or $Action -cnotin @('note-menu','copy-link')){throw 'Unknown fixed action'}
 $name=$Stage+'-'+$Action
 $request=Join-Path $Directory ($name+'-request.json')
 $approval=Join-Path $Directory ($name+'-approval.json')
 if((Test-Path $request) -or (Test-Path $approval)){throw 'Repeated action refused'}
 $frame=SaveCopiedFrame $Directory ($name+'-observe')
 $r=[ordered]@{stage=$Stage;action=$Action;sha256=$frame.sha256;pid=$frame.window.Pid;hwnd=$frame.window.H;left=$frame.window.L;top=$frame.window.T;width=($frame.window.R-$frame.window.L);height=($frame.window.B-$frame.window.T);observed_utc=[DateTime]::UtcNow.ToString('o');frame=$frame.file}
 $r|ConvertTo-Json|Set-Content $request -Encoding UTF8
 Log ('AWAIT COPIED APPROVAL '+$name)
 $clock=[Diagnostics.Stopwatch]::StartNew()
 while(!(Test-Path $approval) -and $clock.Elapsed.TotalSeconds -lt 120){Start-Sleep -Milliseconds 250}
 if(!(Test-Path $approval)){throw 'Copied frame approval deadline'}
 $a=Get-Content $approval -Raw|ConvertFrom-Json
 if($a.inspected_sha256 -cne $frame.sha256 -or $a.stage -cne $Stage -or $a.action -cne $Action){throw 'Wrong copied frame approval'}
 $label=if($Action -eq 'copy-link'){'Copy Okilum link'}else{'Note menu'}
 if($a.label -cne $label){throw 'Wrong personally inspected action label'}
 foreach($key in @('x','y','width','height')){if($a.$key -isnot [int] -and $a.$key -isnot [long]){throw 'Integer inspected bounds required'}}
 if($a.x -lt 0 -or $a.y -lt 0 -or $a.width -lt 2 -or $a.height -lt 2 -or $a.x+$a.width -gt $r.width -or $a.y+$a.height -gt $r.height){throw 'Inspected action outside own window'}
 $current=SaveCopiedFrame $Directory ($name+'-current')
 foreach($key in @('H','Pid','L','T','R','B')){if($current.window.$key -ne $frame.window.$key){throw 'Copied window changed'}}
 if($current.sha256 -cne $frame.sha256){throw 'Copied full frame changed; STOP, no re-observe'}
 $a|ConvertTo-Json|Set-Content (Join-Path $Directory ($name+'-intent.json')) -Encoding UTF8
 Click ($r.left+$a.x+[int][Math]::Floor($a.width/2)) ($r.top+$a.y+[int][Math]::Floor($a.height/2))
 $frame
}
function CopyActualIdentityLink([string]$Directory,[string]$Stage){
 $sentinel='OWNED-CLIPBOARD-CONTROL-'+[Guid]::NewGuid().ToString('N')
 [Windows.Forms.Clipboard]::SetText($sentinel)
 if([Windows.Forms.Clipboard]::GetText() -cne $sentinel){throw 'Clipboard positive control failed'}
 $null=InvokeCopiedMenuAction $Directory $Stage 'note-menu'
 $frame=InvokeCopiedMenuAction $Directory $Stage 'copy-link'
 $clock=[Diagnostics.Stopwatch]::StartNew();$value=$sentinel
 while($value -ceq $sentinel -and $clock.Elapsed.TotalSeconds -lt 10){Start-Sleep -Milliseconds 200;$value=[Windows.Forms.Clipboard]::GetText()}
 if($value -ceq $sentinel -or $value -cnotmatch '^okilum://v/Identity(Seed|Failure)/Home\.md(\?id=[A-Za-z0-9-]{8,64})?$'){throw 'Actual Copy link missing/wrong'}
 [pscustomobject]@{clipboard=$value;frame=$frame;positive_control=$sentinel}
}
function WaitCopiedNote([string]$Directory,[string]$Root,[string]$Note){
 $clock=[Diagnostics.Stopwatch]::StartNew()
 do{
  $w=GetCopiedWindow $Directory
  $state=Get-Content (Join-Path $Directory 'state\open-windows.json') -Raw|ConvertFrom-Json
  $ready=TestVaultReadiness $w $state $Root
  if($ready -and $w.Title -ceq ('Okilum - '+[IO.Path]::GetFileNameWithoutExtension($Note)+' - '+[IO.Path]::GetFileName($Root))){return SaveCopiedFrame $Directory ('note-'+[Guid]::NewGuid().ToString('N'))}
  Start-Sleep -Milliseconds 500
 }while($clock.Elapsed.TotalSeconds -lt 60)
 throw 'Actual copied root/note readiness deadline'
}
