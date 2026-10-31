$ErrorActionPreference='Stop'
$source=[IO.File]::ReadAllText((Join-Path $PSScriptRoot 'driver.ps1'))
$start=$source.IndexOf('function WaitFor(');$end=$source.IndexOf('function AssertApp(', $start)
Invoke-Expression $source.Substring($start,$end-$start)
$condition='native UIA condition sentinel'
$result=WaitFor 'dynamic scope positive control' {if($condition -cne 'native UIA condition sentinel'){throw 'Caller condition shadowed'};'CONTROL_PASS'} 1
if($result -cne 'CONTROL_PASS'){throw 'Probe did not execute'}
$refused=$false
try{WaitFor 'negative' {$false} 0.01}catch{if($_.Exception.Message -notlike 'HARNESS readiness deadline:*'){throw};$refused=$true}
if(!$refused){throw 'Absent result accepted'}
'WAIT_SCOPE_POSITIVE_AND_NEGATIVE_PASS'
