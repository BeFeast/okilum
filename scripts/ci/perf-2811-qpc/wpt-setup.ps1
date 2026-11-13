param([string]$OutputDirectory = 'diagnostics')
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
# The successful earlier captures used this standard Windows SDK WPT directory.
$wpt = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10/Windows Performance Toolkit'
$provisioning = 'existing'
if (-not (Get-Command xperf.exe -ErrorAction SilentlyContinue) -and -not (Test-Path "$wpt/xperf.exe")) {
    # runner-images also uses cached SDK BundleCachePath for optional features:
    # images/windows/scripts/build/Install-VisualStudio.ps1. Install WPT only.
    $sdk = Get-ItemProperty 'HKLM:/Software/Wow6432Node/Microsoft/Windows/CurrentVersion/Uninstall/*' -ErrorAction SilentlyContinue |
        Where-Object { $_.DisplayName -match 'Windows Software Development Kit' -and $_.BundleCachePath -and (Test-Path $_.BundleCachePath) } |
        Sort-Object { [version]$_.DisplayVersion } -Descending | Select-Object -First 1
    if ($sdk) {
        $installer = $sdk.BundleCachePath
        $provisioning = 'cached-sdk-WPT-feature'
    } else {
        # Official Windows 11 SDK bootstrapper used by runner-images. This
        # adds only OptionId.WindowsPerformanceToolkit, never a compiler/SDK set.
        $installer = Join-Path $env:RUNNER_TEMP 'qpc-wpt-sdksetup.exe'
        Invoke-WebRequest 'https://go.microsoft.com/fwlink/?linkid=2349110' -OutFile $installer
        $provisioning = 'official-sdk-WPT-feature'
    }
    $installation = Start-Process -FilePath $installer -ArgumentList @('/q', '/norestart', '/ceip', 'off', '/features', 'OptionId.WindowsPerformanceToolkit') -Wait -PassThru
    if ($installation.ExitCode -notin @(0, 3010)) { throw "WPT feature installation failed: $($installation.ExitCode)" }
}
if (Test-Path "$wpt/xperf.exe") { $env:PATH = "$wpt;$env:PATH" }
$xperf = Get-Command xperf.exe -ErrorAction Stop
$tracerpt = Get-Command tracerpt.exe -ErrorAction Stop
$version = (Get-Item $xperf.Source).VersionInfo.FileVersion
if (-not $version) { throw 'xperf file version is unavailable' }
$helpText = & $xperf.Source -help start 2>&1 | Out-String
$helpExit = $LASTEXITCODE
$helpText | Set-Content "$OutputDirectory/xperf-start-help.txt"
if ($helpExit -or $helpText -notmatch 'clocktype' -or $helpText -notmatch 'stackwalk') {
    throw 'xperf start options could not be verified before capture'
}
@{ provisioning = $provisioning; xperf_path = $xperf.Source; xperf_file_version = $version;
   xperf_sha256 = (Get-FileHash $xperf.Source -Algorithm SHA256).Hash;
   tracerpt_path = $tracerpt.Source; tracerpt_file_version = (Get-Item $tracerpt.Source).VersionInfo.FileVersion;
   help_exit = $helpExit; setup_completed_before_capture = $true } |
    ConvertTo-Json | Set-Content "$OutputDirectory/wpt-setup.json"
