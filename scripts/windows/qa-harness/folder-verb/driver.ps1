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
$script:folderExplorers=@()
$hashes=@{}
foreach($folder in $folders){$hashes[$folder]=(Get-FileHash (Join-Path $folder 'Home.md')).Hash}
function ReadJson($path){if(Test-Path $path){[IO.File]::ReadAllText($path)|ConvertFrom-Json}}
function SaveJson($name,$value){ConvertTo-Json -InputObject $value -Depth 12|Set-Content (Join-Path $fixture $name) -Encoding UTF8}
function WaitFor($name,[scriptblock]$Probe,$seconds=120){
 $clock=[Diagnostics.Stopwatch]::StartNew()
 while($clock.Elapsed.TotalSeconds -lt $seconds){$r=&$Probe;if($r){return $r};Start-Sleep -Milliseconds 1000}
 throw ('HARNESS readiness deadline: '+$name)
}
function AssertApp($folder){
 $tag=if($folder -ceq $folders[0]){'cold'}else{'warm'}
 $id=ReadJson (Join-Path $state 'reader-instance.json');$windows=ReadJson (Join-Path $state 'open-windows.json')
 $facts=ReadJson (Join-Path $state 'whats-new.json')
 $census=@(Get-Process okilum -EA SilentlyContinue|ForEach-Object {
  $row=@{pid=$_.Id;session=$_.SessionId;start_utc=$_.StartTime.ToUniversalTime().ToString('o')}
  try{$row.exe=$_.MainModule.FileName;$row.sha256=(Get-FileHash $row.exe).Hash.ToLowerInvariant();$v=[Diagnostics.FileVersionInfo]::GetVersionInfo($row.exe);$row.version_info=@{file=$v.FileVersion;product=$v.ProductVersion}}catch{$row.observation_error=$_.Exception.Message}
  $row
 })
 $snapshot=@{utc=[DateTime]::UtcNow.ToString('o');tag=$tag;requested_folder=$folder;reader_instance=$id;windows=$windows;facts=$facts;processes=$census;sq_version=if(Test-Path "$install/current/sq.version"){[IO.File]::ReadAllText("$install/current/sq.version")}}
 SaveJson ($tag+'-identity.json') $snapshot
 (ConvertTo-Json -InputObject $snapshot -Depth 12 -Compress)|Add-Content (Join-Path $fixture 'identity-samples.jsonl') -Encoding UTF8
 if(!$id -or !$windows -or @($windows.roots).Count -ne 1){return $null}
 if([IO.Path]::GetFullPath(([string]$windows.roots[0]).Replace('\\?\','')).TrimEnd('\') -ine [IO.Path]::GetFullPath($folder).TrimEnd('\')){return $null}
 $observed=@($census|Where-Object {$_.pid -eq $id.pid})
 if(!$observed.Count){return $null};$actual=$observed[0]
 if($actual.observation_error){throw ('HARNESS: identity observation failed '+$actual.observation_error)}
 if($actual.session -ne 1 -or !$actual.exe.StartsWith($install+'\',[StringComparison]::OrdinalIgnoreCase)){throw 'Foreign app identity'}
 if($actual.sha256 -cne $c.exe_sha256){throw 'PRODUCT FAIL: wrong installed executable; actual identity retained'}
 if(!$facts){return $null}
 if($facts.source -cne $c.source){throw 'PRODUCT FAIL: wrong runtime source'}
 $w=Wins|Where-Object {$_.Pid -eq $actual.pid -and $_.Class -eq 'Zed::Window'}|Select-Object -First 1
 if(!$w){return $null}
 $app=[Windows.Automation.AutomationElement]::FromHandle($w.H)
 $names=@($app.FindAll([Windows.Automation.TreeScope]::Descendants,[Windows.Automation.Condition]::TrueCondition)|ForEach-Object {$_.Current.Name})
 SaveJson ($tag+'-app-ui-names.json') $names
 if('Projects' -cnotin $names){return $null} # Positive control for toast observation.
 if('Restart now' -cin $names -or 'Update ready' -cin $names -or (Test-Path (Join-Path $state 'windows-update-verified'))){throw 'HARNESS: Update ready appeared despite isolation'}
 $identity=@{pid=$actual.pid;exe=$actual.exe;sha256=$actual.sha256;version_info=$actual.version_info;start_utc=$actual.start_utc;source=$facts.source;folder=$folder;window=$w;process_count=$census.Count;update_ready=$false}
 SaveJson ($tag+'-accepted-identity.json') $identity
 $identity
}
function CaptureRect($name,$rect){
 if($rect.Width -lt 2 -or $rect.Height -lt 2){throw 'Invalid menu frame bounds'}
 $bitmap=New-Object Drawing.Bitmap ([int]$rect.Width),([int]$rect.Height)
 $graphics=[Drawing.Graphics]::FromImage($bitmap)
 try{$graphics.CopyFromScreen([int]$rect.X,[int]$rect.Y,0,0,$bitmap.Size);$bitmap.Save("$T\$name.png")}finally{$graphics.Dispose();$bitmap.Dispose()}
 Log ('MENU FRAME '+$name)
}
function CaptureFolderMenu($folder,$tag){
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
 $script:folderExplorers+=,$window
 $p=Get-Process -Id $w.Pid
 if($p.SessionId -ne 1 -or $p.MainModule.FileName -ine (Join-Path $env:windir 'explorer.exe')){throw 'Foreign Explorer owner'}
 Focus $w
 $view=[Windows.Automation.AutomationElement]::FromHandle($w.H)
 $folderCondition=New-Object Windows.Automation.PropertyCondition ([Windows.Automation.AutomationElement]::NameProperty),([IO.Path]::GetFileName($folder))
 $selected=WaitFor 'selected Explorer folder accessibility' {
  $items=@($view.FindAll([Windows.Automation.TreeScope]::Descendants,$folderCondition)|Where-Object {$_.Current.ProcessId -eq $w.Pid -and $_.Current.ControlType -cin @([Windows.Automation.ControlType]::ListItem,[Windows.Automation.ControlType]::DataItem)})
  if($items.Count -gt 1){throw 'Ambiguous selected Explorer item'};if($items.Count -eq 1){$items[0]}
 }
 $selected.SetFocus()
 $focused=[Windows.Automation.AutomationElement]::FocusedElement
 if($focused.Current.Name -cne [IO.Path]::GetFileName($folder) -or $focused.Current.ProcessId -ne $w.Pid){throw 'HARNESS: keyboard focus is not selected folder'}
 Shot ($tag+'-explorer')
 Keys '+{F10}' 500
 $root=[Windows.Automation.AutomationElement]::RootElement
 $focusedMenu=[Windows.Automation.AutomationElement]::FocusedElement
 SaveJson ($tag+'-menu-focus.json') @{name=$focusedMenu.Current.Name;type=$focusedMenu.Current.ControlType.ProgrammaticName;pid=$focusedMenu.Current.ProcessId}
 try{
 $moreCondition=New-Object Windows.Automation.PropertyCondition ([Windows.Automation.AutomationElement]::NameProperty),'Show more options'
 $more=@($root.FindAll([Windows.Automation.TreeScope]::Descendants,$moreCondition)|Where-Object {$_.Current.ProcessId -eq $w.Pid -and !$_.Current.IsOffscreen -and $_.Current.IsEnabled -and $_.Current.ControlType -cin @([Windows.Automation.ControlType]::MenuItem,[Windows.Automation.ControlType]::Button)})
 SaveJson ($tag+'-more-options.json') @($more|ForEach-Object {@{name=$_.Current.Name;type=$_.Current.ControlType.ProgrammaticName;pid=$_.Current.ProcessId;bounds=$_.Current.BoundingRectangle}})
 if($more.Count -gt 1){throw 'Ambiguous Show more options'}
 if($more.Count -eq 1){
  $parent=[Windows.Automation.TreeWalker]::ControlViewWalker.GetParent($more[0])
  CaptureRect ($tag+'-modern-menu') $parent.Current.BoundingRectangle
  $invokeMore=$null
  if(!$more[0].TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern,[ref]$invokeMore)){throw 'HARNESS: Show more options lacks InvokePattern'}
  $invokeMore.Invoke();Start-Sleep -Milliseconds 500
 }
 $menuCondition=New-Object Windows.Automation.PropertyCondition ([Windows.Automation.AutomationElement]::NameProperty),'Open in Okilum as Vault'
 $entry=WaitFor 'actual Explorer menu item' {
  $matches=@($root.FindAll([Windows.Automation.TreeScope]::Descendants,$menuCondition)|Where-Object {$_.Current.ProcessId -eq $w.Pid -and $_.Current.IsEnabled -and !$_.Current.IsOffscreen -and $_.Current.ControlType -cin @([Windows.Automation.ControlType]::MenuItem,[Windows.Automation.ControlType]::Button)})
  if($matches.Count -gt 1){throw 'Ambiguous Explorer verb'};if($matches.Count -eq 1){$matches[0]}
 } 5
 }catch{
  $all=@($root.FindAll([Windows.Automation.TreeScope]::Descendants,[Windows.Automation.Condition]::TrueCondition)|Where-Object {!$_.Current.IsOffscreen}|ForEach-Object {@{name=$_.Current.Name;type=$_.Current.ControlType.ProgrammaticName;pid=$_.Current.ProcessId;bounds=$_.Current.BoundingRectangle}})
  SaveJson ($tag+'-menu-census.json') $all
  SaveJson ($tag+'-windows-census.json') @(Wins)
  Shot ($tag+'-context-surface') $w
  foreach($popup in @(Wins|Where-Object {$_.Class -eq '#32768'})){Shot ($tag+'-popup-'+$popup.H) $popup}
  throw
 }
 $walker=[Windows.Automation.TreeWalker]::ControlViewWalker;$menu=$walker.GetParent($entry)
 CaptureRect ($tag+'-visible-menu') $menu.Current.BoundingRectangle
 SaveJson ($tag+'-menu.json') @{name=$entry.Current.Name;explorer_pid=$w.Pid;selected=$folder;bounds=$entry.Current.BoundingRectangle}
 $window
}
function FolderVerbs($folder,$tag){
 $shell=New-Object -ComObject Shell.Application
 $namespace=$shell.Namespace([IO.Path]::GetDirectoryName($folder))
 if(!$namespace){throw 'HARNESS: parent namespace unavailable'}
 $item=$namespace.ParseName([IO.Path]::GetFileName($folder))
 if(!$item -or $item.Path -ine $folder){throw 'HARNESS: wrong Shell folder item'}
 $verbs=@($item.Verbs())
 SaveJson ($tag+'-directory-verbs.json') @($verbs|ForEach-Object {@{name=$_.Name;normalized=$_.Name.Replace('&','').Trim()}})
 # Self describes the folder item; this is not Directory/Background runtime evidence.
 $self=$shell.Namespace($folder).Self
 SaveJson ($tag+'-self-verbs.json') @($self.Verbs()|ForEach-Object {$_.Name})
 $matches=@($verbs|Where-Object {$_.Name.Replace('&','').Trim() -ceq 'Open in Okilum as Vault'})
 if($matches.Count -gt 1){throw 'PRODUCT FAIL: ambiguous Directory shell verb'}
 @{item=$item;matches=$matches}
}
function OpenFolderVerb($folder,$tag){
 $found=FolderVerbs $folder $tag
 if($found.matches.Count -ne 1){throw 'PRODUCT FAIL: installed Directory shell verb absent'}
 # Visual menu evidence is optional; it never dispatches the verb or gates runtime.
 try{$visual=CaptureFolderMenu $folder $tag}catch{SaveJson ($tag+'-visual-warning.json') @{error=$_.Exception.Message};Log ('VISUAL ONLY: '+$_.Exception.Message)}
 Keys '{ESC}' 200
 $found.matches[0].DoIt()
 Log ('DIRECTORY_VERB_DISPATCHED '+$tag)
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
 OpenFolderVerb $folders[0] 'cold'
 $cold=WaitFor 'cold vault open' {AssertApp $folders[0]};Shot 'cold-vault' $cold.window
 OpenFolderVerb $folders[1] 'warm'
 $warm=WaitFor 'warm vault handoff' {AssertApp $folders[1]}
 if($warm.pid -ne $cold.pid -or $warm.start_utc -cne $cold.start_utc){throw 'PRODUCT FAIL: warm open started a second owner'}
 if($warm.process_count -ne 1){throw 'PRODUCT FAIL: multiple Okilum processes after warm handoff'}
 Shot 'warm-vault' $warm.window
 SaveJson 'open-results.json' @{cold=$cold;warm=$warm}
 Focus $warm.window;Keys '%{F4}' 0
 $null=WaitFor 'normal app close' {if(!@(Get-Process okilum -EA SilentlyContinue).Count){$true}}
 SaveJson 'pre-uninstall-executable.json' @{path="$install/current/okilum.exe";sha256=(Get-FileHash "$install/current/okilum.exe").Hash.ToLowerInvariant()}
 if((Get-FileHash "$install/current/okilum.exe").Hash.ToLowerInvariant() -cne $c.exe_sha256){throw 'HARNESS: package changed before uninstall'}
 $update=Join-Path $install 'Update.exe'
 $p=Start-Process $update -ArgumentList '--uninstall','--silent' -PassThru
 if(!$p.WaitForExit(180000) -or $p.ExitCode -ne 0){throw 'PRODUCT FAIL: uninstall failed'}
 foreach($folder in $folders){$after=FolderVerbs $folder 'uninstalled';if($after.matches.Count){throw 'PRODUCT FAIL: uninstalled Directory shell verb remains'}}
 foreach($key in @($keys)+@('HKCU:\Software\Classes\BeFeast.Okilum.Markdown','HKCU:\Software\Classes\okilum')){
  if(Test-Path $key){throw ('PRODUCT FAIL: registration remains '+$key)}
 }
 if(!(Test-Path $neighbor)){throw 'PRODUCT FAIL: uninstall removed neighboring verb'}
 foreach($key in $uninstallKeys){if(Test-Path $key){throw 'PRODUCT FAIL: uninstall entry remains'}}
 foreach($folder in $folders){if((Get-FileHash (Join-Path $folder 'Home.md')).Hash -cne $hashes[$folder]){throw 'PRODUCT FAIL: uninstall changed vault bytes'}}
 SaveJson 'result.json' @{status='EXECUTED_PENDING_FRAME_REVIEW';source=$c.source;installed_verbs=$listed;cold=$cold;warm=$warm;uninstalled=$true;owned_registration_keys=0;background_runtime='NOTRUN; follow-up #1914';directory_dispatch='Shell.Application FolderItem.Verbs DoIt';neighbor_preserved=$true;vault_hashes=$hashes}
 Log 'INSTALL_FOLDER_VERB_UNINSTALL_PASS'
}finally{
 if(Test-Path $neighbor){Remove-Item $neighbor -Force}
 # Only windows opened by this scenario, never the desktop Explorer process.
 foreach($window in $script:folderExplorers|Where-Object {$_}){try{$window.Quit()}catch{Log ('Explorer cleanup '+$_)}}
}
