$ErrorActionPreference='Stop'
$fixture=Join-Path $env:GITHUB_WORKSPACE 'scripts/qa/windows-network'
$probe=Join-Path $env:GITHUB_WORKSPACE 'target/debug/examples/updater_probe.exe'
$receipt=Join-Path $env:GITHUB_WORKSPACE 'updater-network-receipt'
New-Item -ItemType Directory -Force $receipt|Out-Null
$reg='HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings'
$before=Get-ItemProperty $reg
$before|Select ProxyEnable,ProxyServer,ProxyOverride,AutoConfigURL|ConvertTo-Json|Set-Content "$receipt/proxy-before.json"
$server=$null;$cert=$null
function InvokeProbe($mode,$endpoint,$name,$success){
 Write-Host "STAGE probe $name begin"
 $output=Join-Path $receipt ($name+'.txt')
 $process=Start-Process $probe -ArgumentList $mode,$endpoint,$output -PassThru -RedirectStandardOutput "$receipt/$name-stdout.txt" -RedirectStandardError "$receipt/$name-stderr.txt"
 if(!$process.WaitForExit(60000)){Stop-Process -Id $process.Id -Force;throw "Probe $name exceeded60s"}
 $process.WaitForExit()
 if(($process.ExitCode -eq 0) -ne $success){throw "Unexpected exit for $name : $($process.ExitCode)"}
 Write-Host "STAGE probe $name complete exit=$($process.ExitCode)"
}
try{
 foreach($name in @('HTTP_PROXY','HTTPS_PROXY','ALL_PROXY','NO_PROXY','http_proxy','https_proxy','all_proxy','no_proxy')){Remove-Item "Env:$name" -EA SilentlyContinue}
 $server=Start-Process python -ArgumentList '-u',"$fixture/proxy.py" -PassThru -RedirectStandardError "$receipt/proxy-error.txt"
 $clock=[Diagnostics.Stopwatch]::StartNew()
 while(!(Test-Path "$fixture/ready.json") -and $clock.Elapsed.TotalSeconds -lt 30){Start-Sleep -Milliseconds 200}
 if(!(Test-Path "$fixture/ready.json")){throw 'Fixture not ready'}
 Set-ItemProperty $reg ProxyEnable 1;Set-ItemProperty $reg ProxyServer '127.0.0.1:18178';Set-ItemProperty $reg ProxyOverride '';Remove-ItemProperty $reg AutoConfigURL -EA SilentlyContinue
 InvokeProbe fixed 'https://updater.okilum.test/updates' untrusted $false
 if((Get-Content "$receipt/untrusted.txt" -Raw) -notmatch 'certificate|TLS|SSL|untrusted|unknownissuer'){throw 'Negative control did not establish a TLS trust rejection'}
 Write-Host 'STAGE machine-root import begin'
 $import=Start-Process certutil.exe -ArgumentList '-f','-addstore','Root',"$fixture/root.cer" -PassThru -RedirectStandardOutput "$receipt/root-import.txt" -RedirectStandardError "$receipt/root-import-error.txt"
 if(!$import.WaitForExit(30000)){Stop-Process -Id $import.Id -Force;throw 'Root import exceeded30s'}
 $import.WaitForExit();if($import.ExitCode -ne 0){throw 'Root import failed; hosted admin required'}
 $cert=[Security.Cryptography.X509Certificates.X509Certificate2]::new("$fixture/root.cer")
 Write-Host 'STAGE machine-root import complete'
 Set-ItemProperty $reg ProxyEnable 0
 InvokeProbe fixed 'https://localhost:18179/updates' trusted-direct $true
 InvokeProbe legacy 'https://localhost:18179/updates' legacy-bundled-root $false
 if((Get-Content "$receipt/legacy-bundled-root.txt" -Raw) -notmatch 'certificate|TLS|SSL|untrusted|unknownissuer'){throw 'Legacy negative was not a TLS trust rejection'}
 Set-ItemProperty $reg ProxyEnable 1
 InvokeProbe fixed 'https://updater.okilum.test/updates' system-static $true
 Set-ItemProperty $reg ProxyEnable 0;Set-ItemProperty $reg AutoConfigURL 'http://127.0.0.1:18178/proxy.pac'
 InvokeProbe fixed 'https://updater.okilum.test/updates' system-pac $true
 $env:GITHUB_SHA|Set-Content "$receipt/source.txt"
 Get-Content "$receipt/system-static.txt","$receipt/system-pac.txt"|Add-Content $env:GITHUB_STEP_SUMMARY
}finally{
 Copy-Item "$fixture/requests.jsonl" $receipt -EA SilentlyContinue

 if($server){Stop-Process -Id $server.Id -Force -EA SilentlyContinue}
 if($cert){Remove-Item "Cert:\LocalMachine\Root\$($cert.Thumbprint)" -EA SilentlyContinue}
 foreach($name in @('ProxyEnable','ProxyServer','ProxyOverride','AutoConfigURL')){
  if($before.PSObject.Properties.Name -contains $name){Set-ItemProperty $reg $name $before.$name}else{Remove-ItemProperty $reg $name -EA SilentlyContinue}
 }
}
