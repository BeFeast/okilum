# Run native mutation/recovery controls with every tempfile on each filesystem.
# Only unavailable ReFS formatting is a skip; compilation/test failures are fatal.
param([switch]$ExistingReFS, [string]$ReFSRoot = '')
$ErrorActionPreference = 'Stop'
$originalTemp = $env:TEMP
$originalTmp = $env:TMP
$base = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { $env:TEMP }
$owned = Join-Path $base ('okilum-filesystems-' + [guid]::NewGuid())
New-Item -ItemType Directory $owned | Out-Null
$vhd = Join-Path $owned 'refs.vhdx'
$attached = $false
function Log($line) {
    Write-Host $line
    if ($env:GITHUB_STEP_SUMMARY) { Add-Content $env:GITHUB_STEP_SUMMARY $line }
}
function Suites($label, $root) {
    $env:TEMP = $root; $env:TMP = $root
    New-Item -ItemType Directory -Force $root | Out-Null
    Log "START $label native save/move/copy/delete/recycle/recovery/identity controls; TEMP=$root; source=$env:GITHUB_SHA"
    python scripts/ci/windows-recovery-check.py
    if ($LASTEXITCODE -ne 0) { throw "$label Windows regression suite failed ($LASTEXITCODE)" }
    foreach ($filter in @('note_move', 'note_files', 'source_history')) {
        $discovered = & cargo test --locked -p okilum-core --tests $filter -- --list
        if ($LASTEXITCODE -ne 0 -or !($discovered -match ': test$')) { throw "$label $filter compiled/discovered no tests" }
        & cargo test --locked -p okilum-core --tests $filter -- --nocapture --test-threads=1
        if ($LASTEXITCODE -ne 0) { throw "$label $filter failed ($LASTEXITCODE)" }
    }
    & cargo test --locked -p okilum-write-authority -- --nocapture --test-threads=1
    if ($LASTEXITCODE -ne 0) { throw "$label writer authority failed ($LASTEXITCODE)" }
    Log "PASS $label native mutation/recovery/identity controls"
}
try {
    Suites 'NTFS' (Join-Path $owned 'ntfs')
    if ($ExistingReFS) {
        if (!$ReFSRoot -or (Get-Volume -FilePath $ReFSRoot).FileSystem -ne 'ReFS') { throw 'A real ReFS path is required' }
        Suites 'ReFS' (Join-Path $ReFSRoot ('okilum-tests-' + [guid]::NewGuid()))
    } else {
        $letter = @('R','S','T','U','V','W','X','Y','Z') | Where-Object { !(Test-Path "${_}:\") } | Select-Object -First 1
        if (!$letter) { Log 'SKIP ReFS: no unused drive letter is available'; return }
        $diskpart = Join-Path $owned 'create.txt'
        @("create vdisk file=`"$vhd`" maximum=4096 type=expandable", "select vdisk file=`"$vhd`"", 'attach vdisk', 'create partition primary', "assign letter=$letter", 'exit') | Set-Content $diskpart
        try {
            & diskpart /s $diskpart | Out-Host
            $attached = Test-Path $vhd
            # An unformatted partition has no accessible root yet. Format it
            # before testing the path, otherwise every capable runner would skip.
            Get-Volume -DriveLetter $letter -ErrorAction Stop | Out-Host
            Format-Volume -DriveLetter $letter -FileSystem ReFS -NewFileSystemLabel OkilumReFSTests -Confirm:$false -ErrorAction Stop | Out-Host
            if ((Get-Volume -DriveLetter $letter).FileSystem -ne 'ReFS') { throw 'Formatted volume is not ReFS' }
        } catch {
            Log "SKIP ReFS: runner cannot create/format a ReFS VHDX: $($_.Exception.Message)"
            return
        }
        Suites 'ReFS' "${letter}:\okilum-tests"
    }
} finally {
    $env:TEMP = $originalTemp; $env:TMP = $originalTmp
    if ($attached) {
        $detach = Join-Path $owned 'detach.txt'
        @("select vdisk file=`"$vhd`"", 'detach vdisk', 'exit') | Set-Content $detach
        & diskpart /s $detach | Out-Host
        if ($LASTEXITCODE -ne 0) { throw 'Owned ReFS VHDX detach failed' }
    }
    # All paths under this UUID directory were created by this invocation.
    Remove-Item -LiteralPath $owned -Recurse -Force
}
