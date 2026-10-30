# Fixed installed-PR Explorer verb scenario, run only through canonical run-evidence.
$ErrorActionPreference='Stop'
Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes
$c=Get-Content "$Q\folder-verb\attempt.json" -Raw -Encoding UTF8|ConvertFrom-Json
if($c.source -cnotmatch '^[a-f0-9]{40}$' -or $c.nonce -cnotmatch '^[a-f0-9]{32}$'){throw 'Invalid admission'}
if((Get-Process -Id $PID).SessionId -ne 1){throw 'Session1 required'}
$install=Join-Path $env:LOCALAPPDATA 'BeFeast.Okilum';$state=Join-Path $env:LOCALAPPDATA 'okilum'
if((Test-Path $install) -or @(Get-Process okilum -EA SilentlyContinue).Count){throw 'Expected clean installed-app state'}
$fixture=Join-Path $Q ('folder-verb-'+$c.nonce);New-Item -ItemType Directory $fixture|Out-Null
$keys=@('HKCU:\Software\Classes\Directory\shell\BeFeast.Okilum.OpenVault','HKCU:\Software\Classes\Directory\Background\shell\BeFeast.Okilum.OpenVault')
foreach($key in $keys){if(Test-Path $key){throw 'Verb present before install'}}
$neighbor='HKCU:\Software\Classes\Directory\shell\QaNeighbor1914';New-Item $neighbor -Force|Out-Null
Set-Item $neighbor 'QA neighbor control'
$folders=@((Join-Path $fixture 'Cold vault with spaces'),(Join-Path $fixture ('Warm vault '+[char]0x03A9)))
foreach($folder in $folders){New-Item -ItemType Directory $folder|Out-Null;[IO.File]::WriteAllText((Join-Path $folder 'Home.md'),('# Fixture '+[IO.Path]::GetFileName($folder)+"`n`nCanonical folder verb control.`n"))}
$hashes=@{}
foreach($folder in $folders){$hashes[$folder]=(Get-FileHash (Join-Path $folder 'Home.md')).Hash}
function ReadJson($path){if(Test-Path $path){[IO.File]::ReadAllText($path)|ConvertFrom-Json}}
function SaveJson($name,$value){ConvertTo-Json -InputObject $value -Depth 12|Set-Content (Join-Path $fixture $name) -Encoding UTF8}
function WaitFor($name,[scriptblock]$condition,$seconds=120){
 $clock=[Diagnostics.Stopwatch]::StartNew()
 while($clock.Elapsed.TotalSeconds -lt $seconds){$r=&$condition;if($r){return $r};Start-Sleep -Milliseconds 300}
 throw ('HARNESS readiness deadline: '+$name)
}
function AssertApp($folder){
 $id=ReadJson (Join-Path $state 'reader-instance.json');$windows=ReadJson (Join-Path $state 'open-windows.json')
 if(!$id -or !$windows -or @($windows.roots).Count -ne 1){return $null}
 if([IO.Path]::GetFullPath(([string]$windows.roots[0]).Replace('\\?\','')).TrimEnd('\') -ine [IO.Path]::GetFullPath($folder).TrimEnd('\')){return $null}
 $p=Get-Process -Id $id.pid -EA SilentlyContinue;if(!$p){return $null}
 if($p.SessionId -ne 1 -or !$p.MainModule.FileName.StartsWith($install+'\',[StringComparison]::OrdinalIgnoreCase)){throw 'Foreign app identity'}
 if((Get-FileHash $p.MainModule.FileName).Hash.ToLowerInvariant() -cne $c.exe_sha256){throw 'PRODUCT FAIL: wrong installed executable'}
 $facts=ReadJson (Join-Path $state 'whats-new.json');if(!$facts){return $null}
 if($facts.source -cne $c.source){throw 'PRODUCT FAIL: wrong runtime source'}
 $identity=@{pid=$p.Id;exe=$p.MainModule.FileName;start_utc=$p.StartTime.ToUniversalTime().ToString('o');source=$facts.source;folder=$folder}
 SaveJson 'owned-process.json' $identity
 $w=Wins|Where-Object {$_.Pid -eq $p.Id -and $_.Title -like 'Okilum*'}|Select-Object -First 1
 if(!$w){return $null};$identity.window=$w;$identity
}
function CaptureRect($name,$rect){
 if($rect.Width -lt 2 -or $rect.Height -lt 2){throw 'Invalid menu frame bounds'}
 $bitmap=New-Object Drawing.Bitmap ([int]$rect.Width),([int]$rect.Height)
 $graphics=[Drawing.Graphics]::FromImage($bitmap)
 try{$graphics.CopyFromScreen([int]$rect.X,[int]$rect.Y,0,0,$bitmap.Size);$bitmap.Save("$T\$name.png")}finally{$graphics.Dispose();$bitmap.Dispose()}
 Log ('MENU FRAME '+$name)
}
function OpenFolderMenu($folder,$tag){
 $shell=New-Object -ComObject Shell.Application
 Start-Process explorer.exe -ArgumentList ('/select,"'+$folder+'"')
 $parent=[IO.Path]::GetDirectoryName($folder)
 $window=WaitFor 'Explorer selection' {
  @($shell.Windows())|Where-Object {try{([Uri]$_.LocationURL).LocalPath.TrimEnd('\') -ieq $parent.TrimEnd('\')}catch{$false}}|Select-Object -First 1
 }
 $item=$window.Document.Folder.ParseName([IO.Path]::GetFileName($folder));if(!$item){throw 'Explorer fixture item absent'}
 $window.Document.SelectItem($item,29)
 $selection=@($window.Document.SelectedItems());if($selection.Count -ne 1 -or $selection[0].Path -ine $folder){throw 'Foreign Explorer selection'}
 $w=Wins|Where-Object {$_.H -eq [IntPtr]$window.HWND}|Select-Object -First 1
 if(!$w){throw 'Explorer window not observed'}
 $p=Get-Process -Id $w.Pid
 if($p.SessionId -ne 1 -or $p.MainModule.FileName -ine (Join-Path $env:windir 'explorer.exe')){throw 'Foreign Explorer owner'}
 Focus $w;Shot ($tag+'-explorer')
 Keys '+{F10}' 500
 $root=[Windows.Automation.AutomationElement]::RootElement
 $condition=New-Object Windows.Automation.PropertyCondition ([Windows.Automation.AutomationElement]::NameProperty),'Open in Okilum as Vault'
 $entry=WaitFor 'actual Explorer menu item' {
  $matches=@($root.FindAll([Windows.Automation.TreeScope]::Descendants,$condition)|Where-Object {$_.Current.ProcessId -eq $w.Pid -and $_.Current.IsEnabled -and $_.Current.ControlType -eq [Windows.Automation.ControlType]::MenuItem})
  if($matches.Count -gt 1){throw 'Ambiguous Explorer verb'};if($matches.Count -eq 1){$matches[0]}
 }
 $walker=[Windows.Automation.TreeWalker]::ControlViewWalker;$menu=$walker.GetParent($entry)
 CaptureRect ($tag+'-visible-menu') $menu.Current.BoundingRectangle
 SaveJson ($tag+'-menu.json') @{name=$entry.Current.Name;explorer_pid=$w.Pid;selected=$folder;bounds=$entry.Current.BoundingRectangle}
 $pattern=$null
 if(!$entry.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern,[ref]$pattern)){throw 'HARNESS: Explorer menu lacks InvokePattern'}
 $pattern.Invoke()
 $window
}
try{
 $setup="$Q\folder-verb\Setup.exe"
 if((Get-FileHash $setup).Hash.ToLowerInvariant() -cne $c.setup_sha256){throw 'Setup digest mismatch'}
 if((Get-AuthenticodeSignature $setup).Status -ne 'NotSigned'){throw 'Expected unsigned exact-head PR Setup'}
 # Pinned Velopack1.2.161 silent install runs install hooks but does not launch app.
 $p=Start-Process $setup -ArgumentList '--silent' -PassThru
 if(!$p.WaitForExit(180000) -or $p.ExitCode -ne 0){throw 'PRODUCT FAIL: silent installation failed'}
 if(@(Get-Process okilum -EA SilentlyContinue).Count){throw 'HARNESS: silent install launched app'}
 $listed=@()
 for($i=0;$i -lt 2;$i++){
  $key=$keys[$i];if(!(Test-Path $key)){throw 'PRODUCT FAIL: installed verb missing'}
  $command=(Get-Item ($key+'\command')).GetValue('');$token=@('%1','%V')[$i]
  if((Get-Item $key).GetValue('') -cne 'Open in Okilum as Vault' -or $command -notmatch '^"([^"]+)" --vault '){throw 'PRODUCT FAIL: wrong verb declaration'}
  $registered=$Matches[1]
  if(!$registered.StartsWith($install+'\',[StringComparison]::OrdinalIgnoreCase) -or (Get-FileHash $registered).Hash.ToLowerInvariant() -cne $c.exe_sha256){throw 'PRODUCT FAIL: verb targets foreign bytes'}
  if(!$command.EndsWith(('"'+$token+'\."'))){throw 'PRODUCT FAIL: wrong folder placeholder'}
  $listed+=@{key=$key;command=$command}
 }
 SaveJson 'installed-verbs.json' $listed
 $uninstallKeys=@(Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall'|Where-Object {
  $v=Get-ItemProperty $_.PSPath
  ($v.InstallLocation -and ([string]$v.InstallLocation).TrimEnd('\') -ieq $install) -or ($_.PSChildName -eq 'BeFeast.Okilum')
 }|ForEach-Object {$_.PSPath})
 if($uninstallKeys.Count -ne 1){throw 'PRODUCT FAIL: uninstall registration missing or ambiguous'}
 SaveJson 'installed-uninstall-keys.json' $uninstallKeys
 New-Item -ItemType Directory $state -Force|Out-Null
 if(Test-Path (Join-Path $state 'reader-ui.json')){throw 'Unexpected preexisting app preferences'}
 # Ordinary persisted window preference makes the warm handoff observable A→B.
 [IO.File]::WriteAllText((Join-Path $state 'reader-ui.json'),'{"version":1,"vault_open":"current"}')
 $coldExplorer=OpenFolderMenu $folders[0] 'cold'
 $cold=WaitFor 'cold vault open' {AssertApp $folders[0]};Shot 'cold-vault' $cold.window
 $warmExplorer=OpenFolderMenu $folders[1] 'warm'
 $warm=WaitFor 'warm vault handoff' {AssertApp $folders[1]}
 if($warm.pid -ne $cold.pid -or $warm.start_utc -cne $cold.start_utc){throw 'PRODUCT FAIL: warm open started a second owner'}
 Shot 'warm-vault' $warm.window
 SaveJson 'open-results.json' @{cold=$cold;warm=$warm}
 Focus $warm.window;Keys '%{F4}' 0
 $null=WaitFor 'normal app close' {if(!@(Get-Process okilum -EA SilentlyContinue).Count){$true}}
 $update=Join-Path $install 'Update.exe'
 $p=Start-Process $update -ArgumentList '--uninstall','--silent' -PassThru
 if(!$p.WaitForExit(180000) -or $p.ExitCode -ne 0){throw 'PRODUCT FAIL: uninstall failed'}
 foreach($key in @($keys)+@('HKCU:\Software\Classes\BeFeast.Okilum.Markdown','HKCU:\Software\Classes\okilum')){
  if(Test-Path $key){throw ('PRODUCT FAIL: registration remains '+$key)}
 }
 if(!(Test-Path $neighbor)){throw 'PRODUCT FAIL: uninstall removed neighboring verb'}
 foreach($key in $uninstallKeys){if(Test-Path $key){throw 'PRODUCT FAIL: uninstall entry remains'}}
 foreach($folder in $folders){if((Get-FileHash (Join-Path $folder 'Home.md')).Hash -cne $hashes[$folder]){throw 'PRODUCT FAIL: uninstall changed vault bytes'}}
 SaveJson 'result.json' @{status='EXECUTED_PENDING_FRAME_REVIEW';source=$c.source;installed_verbs=$listed;cold=$cold;warm=$warm;uninstalled=$true;owned_registration_keys=0;neighbor_preserved=$true;vault_hashes=$hashes}
 Log 'INSTALL_FOLDER_VERB_UNINSTALL_PASS'
}finally{
 if(Test-Path $neighbor){Remove-Item $neighbor -Force}
 # Only windows opened by this scenario, never the desktop Explorer process.
 foreach($window in @($coldExplorer,$warmExplorer)|Where-Object {$_}){try{$window.Quit()}catch{Log ('Explorer cleanup '+$_)}}
}
