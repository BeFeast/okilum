$ErrorActionPreference='Stop'
$fixture=Join-Path $env:GITHUB_WORKSPACE 'scripts/qa/windows-network'
$probe=Join-Path $env:GITHUB_WORKSPACE 'target/debug/examples/updater_probe.exe'
$receipt=Join-Path $env:GITHUB_WORKSPACE 'updater-network-receipt'
New-Item -ItemType Directory -Force $receipt|Out-Null
$reg='HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings'
$before=Get-ItemProperty $reg
$before|Select ProxyEnable,ProxyServer,ProxyOverride,AutoConfigURL|ConvertTo-Json|Set-Content "$receipt/proxy-before.json"
$server=$null;$cert=$null
try{
 foreach($name in @('HTTP_PROXY','HTTPS_PROXY','ALL_PROXY','NO_PROXY','http_proxy','https_proxy','all_proxy','no_proxy')){Remove-Item "Env:$name" -EA SilentlyContinue}
 $server=Start-Process python -ArgumentList '-u',"$fixture/proxy.py" -PassThru -RedirectStandardError "$receipt/proxy-error.txt"
 $clock=[Diagnostics.Stopwatch]::StartNew()
 while(!(Test-Path "$fixture/ready.json") -and $clock.Elapsed.TotalSeconds -lt 30){Start-Sleep -Milliseconds 200}
 if(!(Test-Path "$fixture/ready.json")){throw 'Fixture not ready'}
 Set-ItemProperty $reg ProxyEnable 1;Set-ItemProperty $reg ProxyServer '127.0.0.1:18178';Set-ItemProperty $reg ProxyOverride '';Remove-ItemProperty $reg AutoConfigURL -EA SilentlyContinue
 & $probe fixed 'https://updater.okilum.test/updates' "$receipt/untrusted.txt"
 if($LASTEXITCODE -eq 0){throw 'Untrusted root unexpectedly accepted'}
 $cert=Import-Certificate -FilePath "$fixture/root.cer" -CertStoreLocation Cert:\CurrentUser\Root
 & $probe legacy 'https://localhost:18179/updates' "$receipt/legacy-bundled-root.txt"
 if($LASTEXITCODE -eq 0){throw 'Legacy bundled trust unexpectedly accepted inspection root'}
 & $probe fixed 'https://updater.okilum.test/updates' "$receipt/system-static.txt"
 if($LASTEXITCODE -ne 0){throw 'Windows trust+system static proxy control failed'}
 Set-ItemProperty $reg ProxyEnable 0;Set-ItemProperty $reg AutoConfigURL 'http://127.0.0.1:18178/proxy.pac'
 & $probe fixed 'https://updater.okilum.test/updates' "$receipt/system-pac.txt"
 if($LASTEXITCODE -ne 0){throw 'Windows trust+system PAC control failed'}
 $env:GITHUB_SHA|Set-Content "$receipt/source.txt"
 Copy-Item "$fixture/requests.jsonl" $receipt
 Get-Content "$receipt/system-static.txt","$receipt/system-pac.txt"|Add-Content $env:GITHUB_STEP_SUMMARY
}finally{
 if($server){Stop-Process -Id $server.Id -Force -EA SilentlyContinue}
 if($cert){Remove-Item "Cert:\CurrentUser\Root\$($cert.Thumbprint)" -EA SilentlyContinue}
 foreach($name in @('ProxyEnable','ProxyServer','ProxyOverride','AutoConfigURL')){
  if($before.PSObject.Properties.Name -contains $name){Set-ItemProperty $reg $name $before.$name}else{Remove-ItemProperty $reg $name -EA SilentlyContinue}
 }
}
