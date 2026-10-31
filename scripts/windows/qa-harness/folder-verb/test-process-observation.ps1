$ErrorActionPreference='Stop'
$tokens=$null;$errors=$null
$ast=[Management.Automation.Language.Parser]::ParseFile((Join-Path $Q 'folder-verb/driver.ps1'),[ref]$tokens,[ref]$errors)
if($errors.Count){throw 'Driver parse failed'}
$definition=$ast.Find({param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'ObserveProcess'},$true)
if(!$definition){throw 'Actual ObserveProcess function not found'}
Invoke-Expression $definition.Extent.Text
$live=ObserveProcess (Get-Process -Id $PID)
if($live.observation_error -or $live.pid -ne $PID -or $live.sha256 -cne (Get-FileHash (Get-Process -Id $PID).MainModule.FileName).Hash.ToLowerInvariant()){throw 'Live observation positive control failed'}
$exited=ObserveProcess ([pscustomobject]@{Id=424242;SessionId=1;StartTime=$null})
if($exited.pid -ne 424242 -or $exited.observation_error -cne 'Process exited before StartTime observation'){throw 'Exited process evidence control failed'}
Log 'PROCESS_OBSERVATION_LIVE_AND_EXITED_PASS'
