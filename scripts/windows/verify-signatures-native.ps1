# Native Authenticode check of a published Windows build (#1104), no secrets.
#
#   verify-signatures-native.ps1 -Build N -CertSha256 <hex> [-Base <url>] [-Work <dir>]
#
# Downloads the public release.json, Setup.exe, full nupkg and portable ZIP of build N,
# checks their hashes against release.json and the feed, then requires for every PE
# (Setup.exe, lib/app/*.exe and *.dll in the nupkg, okilum.exe in the portable ZIP):
#   - signtool verify /pa /all /v /tw exits 0 (2 = warnings, e.g. no timestamp = fail);
#   - Get-AuthenticodeSignature Status Valid with a timestamper certificate;
#   - the signer certificate SHA-256 equals -CertSha256.
# This complements scripts/windows/verify-signatures.py (osslsigncode on Linux) with
# Windows' own verifier: required for the first signed build and before each stable promotion.
param(
    [Parameter(Mandatory = $true)][string]$Build,
    [Parameter(Mandatory = $true)][string]$CertSha256,
    [string]$Base = 'https://updates.befeast.com/okilum/windows',
    [string]$Work = (Join-Path ([IO.Path]::GetTempPath()) "okilum-signatures-$PID")
)
$ErrorActionPreference = 'Stop'
if ($Build -notmatch '^[0-9]{1,9}$') { throw "Build must be a number, got '$Build'" }
$pinned = $CertSha256.ToLowerInvariant()
if ($pinned -notmatch '^[0-9a-f]{64}$') { throw 'CertSha256 must be 64 hex digits' }

function Get-Sha256([string]$Path) { (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant() }

function Save-Url([string]$Url, [string]$Path) {
    Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $Path -MaximumRetryCount 3 -RetryIntervalSec 5
}

New-Item -ItemType Directory -Force -Path $Work | Out-Null
$archive = "$Base/builds/$Build"
$releasePath = Join-Path $Work 'release.json'
Save-Url "$archive/release.json" $releasePath
$release = Get-Content -Raw -LiteralPath $releasePath | ConvertFrom-Json
if ("$($release.build)" -ne $Build) { throw "release.json names build $($release.build), not $Build" }
# CONTROL ONLY: metadata gate removed to exercise signtool on unsigned PEs

$targets = [System.Collections.Generic.List[object]]::new()
$setup = Join-Path $Work 'Setup.exe'
Save-Url "$archive/Setup.exe" $setup
if ((Get-Sha256 $setup) -ne "$($release.setup_sha256)".ToLowerInvariant()) { throw 'Setup.exe hash differs from release.json' }
$targets.Add([pscustomobject]@{Label = 'Setup.exe'; Path = $setup})

# A beta feed also lists earlier full packages; this build's is the one with its version.
$full = @($release.feed.Assets | Where-Object { $_.Type -eq 'Full' -and "$($_.Version)" -eq "$($release.version)" })
if ($full.Count -ne 1) { throw "Expected one full package of version $($release.version) in the feed, found $($full.Count)" }
$name = "$($full[0].FileName)"
if ($name -notmatch '^BeFeast\.Okilum-[0-9][A-Za-z0-9.+-]*-(full)\.nupkg$') { throw "Unexpected package name $name" }
$nupkg = Join-Path $Work $name
# Packages are written to the beta prefix at publication; promotion copies them to stable.
$fetched = $false
foreach ($channel in 'beta', 'stable') {
    try { Save-Url "$Base/$channel/$name" $nupkg; $fetched = $true; break } catch { Write-Host "no $channel/$name" }
}
if (-not $fetched) { throw "Package $name not found" }
if ((Get-Sha256 $nupkg) -ne "$($full[0].SHA256)".ToLowerInvariant()) { throw "$name hash differs from the feed" }

Add-Type -AssemblyName System.IO.Compression.FileSystem
$package = Join-Path $Work 'package'
[IO.Compression.ZipFile]::ExtractToDirectory($nupkg, $package)
$app = Join-Path $package 'lib/app'
$pes = @(Get-ChildItem -LiteralPath $app -Recurse -File | Where-Object { $_.Extension -in '.exe', '.dll' })
if (-not ($pes | Where-Object Name -eq 'okilum.exe')) { throw 'Package has no lib/app/okilum.exe' }
foreach ($pe in $pes) {
    $targets.Add([pscustomobject]@{Label = "$name!lib/app/$($pe.FullName.Substring($app.Length + 1).Replace('\', '/'))"; Path = $pe.FullName})
}

$zip = Join-Path $Work 'Okilum-windows-portable.zip'
try { Save-Url "$archive/Okilum-windows-portable.zip" $zip } catch { $zip = $null; Write-Host 'No portable ZIP for this build' }
if ($zip) {
    $portable = Join-Path $Work 'portable'
    [IO.Compression.ZipFile]::ExtractToDirectory($zip, $portable)
    $exe = @(Get-ChildItem -LiteralPath $portable -Recurse -File -Filter 'okilum.exe')
    if ($exe.Count -ne 1) { throw "Expected one okilum.exe in the portable ZIP, found $($exe.Count)" }
    $targets.Add([pscustomobject]@{Label = 'Okilum-windows-portable.zip!okilum.exe'; Path = $exe[0].FullName})
}

$signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" |
    Sort-Object { [version]$_.Directory.Parent.Name } | Select-Object -Last 1
if (-not $signtool) { throw 'signtool.exe not found in the Windows SDK' }
Write-Host "signtool: $($signtool.FullName)"

$failed = 0
foreach ($t in $targets) {
    $issues = @()
    & $signtool.FullName verify /pa /all /v /tw $t.Path | Out-Host
    if ($LASTEXITCODE -ne 0) { $issues += "signtool exit $LASTEXITCODE" }
    $sig = Get-AuthenticodeSignature -LiteralPath $t.Path
    if ($sig.Status -ne 'Valid') { $issues += "status $($sig.Status)" }
    if (-not $sig.TimeStamperCertificate) { $issues += 'no timestamp' }
    $signer = 'none'
    if ($sig.SignerCertificate) {
        $hash = [Security.Cryptography.SHA256]::Create().ComputeHash($sig.SignerCertificate.RawData)
        $signer = ([BitConverter]::ToString($hash) -replace '-', '').ToLowerInvariant()
        if ($signer -ne $pinned) { $issues += "signer $signer" }
    }
    else { $issues += 'no signer certificate' }
    $who = if ($sig.SignerCertificate) { $sig.SignerCertificate.Subject } else { '-' }
    $tsa = if ($sig.TimeStamperCertificate) { $sig.TimeStamperCertificate.Subject } else { '-' }
    if ($issues) {
        $failed++
        Write-Host "::error::$($t.Label): $($issues -join '; ')"
    }
    else {
        Write-Host "ok $($t.Label): signer '$who', timestamper '$tsa'"
    }
}
Write-Host "Checked $($targets.Count) PE files of build $Build, $failed failed"
if ($failed) { exit 1 }
