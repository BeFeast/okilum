param([string]$InputsDirectory = 'inputs', [string]$OutputDirectory = 'diagnostics')
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
& "$PSScriptRoot/wpt-setup.ps1" -OutputDirectory $OutputDirectory
$provenance = Get-Content "$OutputDirectory/capture-provenance.json" | ConvertFrom-Json
if (-not $provenance.setup_proof) { throw 'Separate reviewed marker setup proof is required before any app launch' }
$wpt = Get-Content "$OutputDirectory/wpt-setup.json" | ConvertFrom-Json
if ($wpt.xperf_sha256.ToLowerInvariant() -ne $provenance.setup_proof.xperf_sha256) { throw 'WPT differs from the proven setup binary' }
# Exactly two --build-info processes, with all harness/control env removed,
# finish BEFORE either ETW session starts. The traced group has only 10 launches.
python scripts/ci/perf-2811-qpc/runner.py --inputs $InputsDirectory --out $OutputDirectory --mode preflight
if ($LASTEXITCODE) { throw 'Immutable native binary preflight failed before ETW start' }
$kernelStarted = $false
$markersStarted = $false
$censusExit = 1
try {
    xperf.exe -on PROC_THREAD+LOADER+CSWITCH+DISPATCHER -ClockType PerfCounter -stackwalk CSwitch+ReadyThread -BufferSize 1024 -MinBuffers 64 -MaxBuffers 256 -f "$OutputDirectory/kernel.raw.etl"
    if ($LASTEXITCODE) { throw 'Kernel scheduler/stack capture failed to start' }
    $kernelStarted = $true
    xperf.exe -start OkilumFrameQpc -on '97057e99-44cf-4e25-aeb7-139ea0cdb951:0x1:4' -ClockType PerfCounter -BufferSize 1024 -MinBuffers 16 -MaxBuffers 64 -f "$OutputDirectory/markers.raw.etl"
    if ($LASTEXITCODE) { throw 'Native frame-marker capture failed to start' }
    $markersStarted = $true
    python scripts/ci/perf-2811-qpc/runner.py --inputs $InputsDirectory --out $OutputDirectory
    $censusExit = $LASTEXITCODE
} finally {
    try {
        if ($markersStarted) {
            xperf.exe -stop OkilumFrameQpc
            if ($LASTEXITCODE) { throw 'Frame-marker capture failed to stop' }
        }
    } finally {
        if ($kernelStarted) {
            xperf.exe -stop 'NT Kernel Logger'
            if ($LASTEXITCODE) { throw 'Kernel capture failed to stop' }
        }
    }
}
xperf.exe -merge "$OutputDirectory/kernel.raw.etl" "$OutputDirectory/markers.raw.etl" "$OutputDirectory/reader.etl"
if ($LASTEXITCODE) { throw 'Raw ETL merge failed' }
# Readability copy only. Attribution uses ProcessTrace RAW_TIMESTAMP exports.
tracerpt.exe "$OutputDirectory/reader.etl" -of XML -o "$OutputDirectory/reader.xml" -y
if ($LASTEXITCODE) { throw 'Readability XML export failed' }
python scripts/ci/perf-2811-qpc/runner.py --inputs $InputsDirectory --out $OutputDirectory --mode postflight
if ($LASTEXITCODE) { throw 'Immutable binary postflight failed after ETW stopped' }
$provenance = Get-Content "$OutputDirectory/capture-provenance.json" | ConvertFrom-Json
@{ census_exit = $censusExit; strict_gate_changed = $false;
   capture_attempt = $provenance.capture_attempt; setup_attempt = $provenance.setup_attempt;
   producer_run = $provenance.producer_run; producer_controller = $provenance.producer_controller;
   capture_controller = $provenance.capture_controller } |
    ConvertTo-Json | Set-Content "$OutputDirectory/capture-state.json"
exit $censusExit
