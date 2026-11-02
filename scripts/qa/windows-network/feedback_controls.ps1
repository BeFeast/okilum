# Hosted, ephemeral Windows runners only. Does not install or launch Okilum.
param([Parameter(Mandatory=$true)][string]$Fixture)
$ErrorActionPreference = 'Stop'
$receipt = Join-Path $env:GITHUB_WORKSPACE 'feedback-network-receipt'
New-Item -ItemType Directory -Force $receipt | Out-Null
$reg = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings'
$before = Get-ItemProperty $reg
$names = @('ProxyEnable', 'ProxyServer', 'ProxyOverride', 'AutoConfigURL')
$proxyEnv = @{}
$server = $null
$cert = $null
$importAttempted = $false
$controls = @()

# Build just the portable production model, without the GUI or updater feature.
$build = & cargo test --locked -p okilum-http --test feedback_model --no-run --message-format=json
if ($LASTEXITCODE) { throw 'Feedback regression build failed' }
$executables = @($build | ForEach-Object {
    $event = $_ | ConvertFrom-Json
    if ($event.reason -eq 'compiler-artifact' -and $event.target.name -eq 'feedback_model' -and $event.executable) {
        $event.executable
    }
})
if ($executables.Count -ne 1) { throw 'Expected one exact-source Feedback test executable' }
$probe = $executables[0]
$test = 'model::tests::corporate_feedback_control'
$listing = & $probe --list --ignored
if ($LASTEXITCODE -or !($listing -match "^${test}: test$")) { throw 'Positive control: fixture test is missing' }

function Invoke-Control($name, $mode, $endpoint, $post, $connect) {
    $name | Set-Content -Encoding utf8 (Join-Path $Fixture 'control.txt')
    $env:OKILUM_CORP_ENDPOINT = $endpoint
    $env:OKILUM_CORP_CONTROL = $mode
    Write-Host "CONTROL begin $name ($mode)"
    $process = Start-Process $probe -ArgumentList @('--exact', $test, '--ignored', '--nocapture') -PassThru `
        -RedirectStandardOutput "$receipt/$name-stdout.txt" -RedirectStandardError "$receipt/$name-stderr.txt"
    try {
        if (!$process.WaitForExit(60000)) { throw "Control $name exceeded 60 seconds" }
        $process.WaitForExit()
        if ($process.ExitCode -ne 0) { throw "Control $name failed: $($process.ExitCode)" }
        $output = Get-Content "$receipt/$name-stdout.txt" -Raw
        if ($output -notmatch '1 passed' -or $output -notmatch 'CONTROL PASS') { throw "Control $name never executed" }
        $events = @(Get-Content "$Fixture/requests.jsonl" | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object control -eq $name)
        $posts = @($events | Where-Object method -eq 'POST')
        $connects = @($events | Where-Object method -eq 'CONNECT')
        if ($posts.Count -ne $post) { throw "Control $name expected $post POSTs, saw $($posts.Count)" }
        if (($connect -and $connects.Count -lt 1) -or (!$connect -and $connects.Count -ne 0)) { throw "Control $name routing was not established" }
        foreach ($request in $posts) {
            if (!$request.valid_multipart -or $request.user_agent -ne 'Okilum/0.1.123 (windows)') { throw "Control $name lost payload/User-Agent" }
        }
        Write-Host $output
        $script:controls += [PSCustomObject]@{name=$name; mode=$mode; posts=$posts.Count; connects=$connects.Count; result='PASS'}
    } finally {
        if (!$process.HasExited) { Stop-Process -Id $process.Id -Force }
    }
}

try {
    foreach ($name in @('HTTP_PROXY','HTTPS_PROXY','ALL_PROXY','NO_PROXY')) {
        $proxyEnv[$name] = [Environment]::GetEnvironmentVariable($name)
        Remove-Item "Env:$name" -ErrorAction SilentlyContinue
    }
    'starting' | Set-Content -Encoding utf8 "$Fixture/control.txt"
    New-Item -ItemType File -Force "$Fixture/requests.jsonl" | Out-Null
    $script = Join-Path $env:GITHUB_WORKSPACE 'scripts/qa/windows-network/feedback_proxy.py'
    $server = Start-Process python -ArgumentList @('-u', "`"$script`"", "`"$Fixture`"") -PassThru `
        -RedirectStandardOutput "$receipt/proxy-stdout.txt" -RedirectStandardError "$receipt/proxy-stderr.txt"
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while (!(Test-Path "$Fixture/ready.json") -and !$server.HasExited -and $clock.Elapsed.TotalSeconds -lt 20) { Start-Sleep -Milliseconds 100 }
    if (!(Test-Path "$Fixture/ready.json")) { throw 'Feedback fixture did not become ready' }
    $ready = Get-Content "$Fixture/ready.json" -Raw | ConvertFrom-Json
    if ($ready.forwarding -ne $false) { throw 'Fixture must never forward requests' }
    $endpoint = 'https://feedback.okilum.test/api/feedback/okilum'
    $direct = "https://localhost:$($ready.direct_port)/api/feedback/okilum"
    Set-ItemProperty $reg ProxyEnable 1
    Set-ItemProperty $reg ProxyServer "http=127.0.0.1:9;https=127.0.0.1:$($ready.proxy_port)"
    Set-ItemProperty $reg ProxyOverride ''
    Remove-ItemProperty $reg AutoConfigURL -ErrorAction SilentlyContinue
    Invoke-Control untrusted untrusted $endpoint 0 $true

    $cert = [Security.Cryptography.X509Certificates.X509Certificate2]::new("$Fixture/root.cer")
    if (Test-Path "Cert:\CurrentUser\Root\$($cert.Thumbprint)") { throw 'Fixture root unexpectedly already present' }
    $importAttempted = $true
    Import-Certificate -FilePath "$Fixture/root.cer" -CertStoreLocation 'Cert:\CurrentUser\Root' | Out-Null
    Set-ItemProperty $reg ProxyEnable 0
    Invoke-Control trusted-direct success $direct 1 $false
    Invoke-Control legacy-root legacy $direct 0 $false
    Set-ItemProperty $reg ProxyEnable 1
    Invoke-Control legacy-proxy legacy-proxy $endpoint 0 $false
    Invoke-Control system-static success $endpoint 1 $true

    Set-ItemProperty $reg ProxyEnable 0
    Set-ItemProperty $reg AutoConfigURL "http://127.0.0.1:$($ready.proxy_port)/proxy.pac"
    Invoke-Control system-pac success $endpoint 1 $true
    $pac = @(Get-Content "$Fixture/requests.jsonl" | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object method -eq 'PAC')
    if (!$pac.Count) { throw 'PAC positive control: no PAC script fetch observed' }
    Invoke-Control proxy-auth proxy-auth $endpoint 0 $true
    Invoke-Control blocked blocked $endpoint 1 $true
    @{source=$env:GITHUB_SHA; controls=$controls; forwarding=$false; trust_store='CurrentUser/Root'} |
        ConvertTo-Json -Depth 5 | Set-Content "$receipt/result.json"
    "Feedback corporate network controls PASS, source=$env:GITHUB_SHA" | Add-Content $env:GITHUB_STEP_SUMMARY
} finally {
    if ($server) { Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue }
    Copy-Item "$Fixture/requests.jsonl" $receipt -ErrorAction SilentlyContinue
    $rootCleanupFailed = $false
    if ($importAttempted) {
        Remove-Item "Cert:\CurrentUser\Root\$($cert.Thumbprint)" -ErrorAction SilentlyContinue
        $rootCleanupFailed = Test-Path "Cert:\CurrentUser\Root\$($cert.Thumbprint)"
    }
    foreach ($name in $names) {
        if ($before.PSObject.Properties.Name -contains $name) { Set-ItemProperty $reg $name $before.$name }
        else { Remove-ItemProperty $reg $name -ErrorAction SilentlyContinue }
    }
    foreach ($name in $proxyEnv.Keys) { [Environment]::SetEnvironmentVariable($name, $proxyEnv[$name]) }
    Remove-Item Env:OKILUM_CORP_ENDPOINT,Env:OKILUM_CORP_CONTROL -ErrorAction SilentlyContinue
    if ($rootCleanupFailed) { throw 'Fixture root cleanup failed' }
}
