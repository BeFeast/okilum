# Frame times of Okilum scrolling a note on a Windows QA VM, repeatable (#1290).
# Run over SSH as qa (an administrator) on VM 151, with the notes from gen-notes.py,
# run-gui.ps1 and gpu-probe.ps1 copied to C:\qa:
#   powershell -File C:\qa\scroll-trace.ps1 -Note C:\qa\notes\big-24k.md -Label beta [-Exe <okilum.exe>] [-Trace]
#
# Per repetition: Okilum starts in qa's console session on the note (own state directory), the
# guest has to be quiet (Defender and the indexer settle after a boot), a console-session helper
# turns the mouse wheel over its window every 16 ms for -Seconds, and an ETW trace of
# Microsoft-Windows-DXGI records every Present of okilum.exe (frame time = interval between
# Present_Start events, PresentMon's MsBetweenPresents). With -Trace the exe is a frame-trace
# build (scripts/perf/build-windows-trace.sh) and also writes its own per-phase trace.log, which
# scripts/perf/scroll/analyze.py reads together with the wheel-start/-end stamps kept here.
# (The switch is $Trace; PowerShell names are case-insensitive, so the trace FILE is $traceFile.)
# A rep whose before/after screenshots are equal did not scroll and is marked invalid.
# The numbers belong to the host: compare only runs from the same VM (AGENTS: one machine).
param(
    [Parameter(Mandatory)][string]$Note,
    [string]$Exe = '',
    [switch]$Trace,
    [int]$Seconds = 10,
    [int]$Reps = 3,
    [string]$Label = 'run',
    [string]$Out = 'C:\qa\bench\scroll',
    [int]$QuietBusyPercent = 12,
    # Wheel notches per second. 60 = one per display refresh, sent against absolute deadlines. With the old
    # loop (-WheelHz 0: Start-Sleep 16 ms per notch) the guest sends only 35-40 a second, which caps the
    # frame rate at that figure once the host can pace 60 Hz (seen on VM 151 with a virtual display).
    [int]$WheelHz = 60
)
$ErrorActionPreference = 'Stop'
$root = Join-Path $Out $Label
New-Item $root -ItemType Directory -Force | Out-Null

# The note must be the probe's own: same bytes on every host.
$manifest = Get-Content (Join-Path (Split-Path $Note) 'manifest.json') -Raw | ConvertFrom-Json
$noteHash = (Get-FileHash $Note -Algorithm SHA256).Hash.ToLower()
$want = $manifest.$((Split-Path $Note -Leaf)).sha256
if ($noteHash -ne $want) { throw "the note is not the probe's: sha256 $noteHash, manifest $want" }

if (-not $Exe) {
    $Exe = Get-ChildItem 'C:\Users\qa\AppData\Local' -Filter okilum.exe -Recurse -Depth 3 -ErrorAction SilentlyContinue |
        Where-Object FullName -like '*\current\*' | Select-Object -First 1 -ExpandProperty FullName
}
if (-not $Exe -or -not (Test-Path $Exe)) { throw 'no okilum.exe: pass -Exe or install Okilum for qa' }
$exeDir = Split-Path $Exe

$video = Get-CimInstance Win32_VideoController | Select-Object -First 1
@(
    "label: $Label   reps: $Reps   seconds: $Seconds   trace: $($Trace.IsPresent)",
    "date: $((Get-Date).ToString('o'))",
    "host: $env:COMPUTERNAME   os: $((Get-CimInstance Win32_OperatingSystem).Caption) $((Get-CimInstance Win32_OperatingSystem).BuildNumber)",
    "cpu: $((Get-CimInstance Win32_Processor | Select-Object -First 1).Name)   logical: $((Get-CimInstance Win32_ComputerSystem).NumberOfLogicalProcessors)",
    "display: $($video.Name)   $($video.CurrentHorizontalResolution)x$($video.CurrentVerticalResolution) @ $($video.CurrentRefreshRate) Hz   driver $($video.DriverVersion)",
    "exe: $Exe   sha256 $((Get-FileHash $Exe -Algorithm SHA256).Hash.ToLower())",
    "note: $Note   sha256 $noteHash   lines $((Get-Content $Note | Measure-Object -Line).Lines)"
) | Set-Content (Join-Path $root 'env.txt')
if (Test-Path (Join-Path $exeDir 'BUILD.txt')) { Get-Content (Join-Path $exeDir 'BUILD.txt') | Add-Content (Join-Path $root 'env.txt') }

# The guest has to be quiet before the wheel turns: a booting Windows runs Defender and the
# search indexer for a while, and that is not Okilum's frame time.
function Wait-Quiet {
    $busy = 100
    foreach ($i in 1..24) {
        $busy = [math]::Round(((Get-Counter '\Processor(_Total)\% Processor Time' -SampleInterval 1 -MaxSamples 3).CounterSamples |
            Measure-Object CookedValue -Average).Average, 1)
        if ($busy -le $QuietBusyPercent) { return @{ quiet = $true; busy = $busy } }
        Start-Sleep 10
    }
    return @{ quiet = $false; busy = $busy }
}

# Can this host pace frames at all? gpui draws once per DwmFlush (its VSyncProvider thread), so a
# host whose DwmFlush takes far longer than a 60 Hz tick caps every app, whatever the note: VM 151
# measured 79 ms per DwmFlush with Okilum closed (12.6 fps for a 20-line note too). The result
# goes into env.txt and every rep says host_capped=True when the median is above 20 ms.
$dwmOut = Join-Path $root 'dwm-flush.txt'
Remove-Item $dwmOut -ErrorAction SilentlyContinue
Get-Process okilum -ErrorAction SilentlyContinue | Stop-Process -Force
& C:\qa\run-gui.ps1 -Program powershell.exe -Arguments "-NoProfile -ExecutionPolicy Bypass -File C:\qa\dwm-flush.ps1 -Count 60 -Out $dwmOut -Label $Label" | Out-Null
foreach ($i in 1..40) { Start-Sleep 2; if ((Test-Path $dwmOut) -and (Select-String -Path $dwmOut -Pattern 'first 12 waits' -Quiet)) { break } }
$dwmP50 = -1
if (Test-Path $dwmOut) {
    $dwmLine = Select-String -Path $dwmOut -Pattern 'DwmFlush x' | Select-Object -First 1 -ExpandProperty Line
    if ($dwmLine -match 'p50 ([\d.]+)') { $dwmP50 = [double]$Matches[1] }
    Add-Content (Join-Path $root 'env.txt') $dwmLine
}
$hostCapped = $dwmP50 -gt 20
"DwmFlush p50: $dwmP50 ms   host_capped=$hostCapped"

# CPU time per thread of the app, to see whether the main thread is busy or waiting.
function Get-ThreadCpu([int]$ProcessId) {
    $threads = @{}
    $proc = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
    if ($proc) {
        foreach ($t in $proc.Threads) {
            try { $threads[$t.Id] = @{ cpu = $t.TotalProcessorTime.TotalMilliseconds; start = $t.StartTime } } catch { }
        }
    }
    $threads
}

foreach ($rep in 1..$Reps) {
    $dir = Join-Path $root "rep$rep"
    Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
    New-Item $dir -ItemType Directory -Force | Out-Null
    Get-Process okilum -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep 2

    # Own state directory: never the qa profile's real Okilum state.
    $state = Join-Path $dir 'state'
    $traceFile = Join-Path $dir 'trace.log'
    $traceLine = if ($Trace) { "set OKILUM_FRAME_TRACE=$traceFile`r`n" } else { '' }
    "@echo off`r`nset OKILUM_STATE_DIR=$state`r`n${traceLine}cd /d `"$exeDir`"`r`nstart `"`" `"$Exe`" `"$Note`"`r`n" |
        Set-Content C:\qa\bench-launch.cmd -Encoding ascii
    & C:\qa\run-gui.ps1 -Program cmd.exe -Arguments '/c C:\qa\bench-launch.cmd' | Out-Null
    # From SSH the window handle reads 0 (other session); the wheel helper looks it up itself.
    foreach ($i in 1..30) {
        Start-Sleep 1
        $p = Get-Process okilum -ErrorAction SilentlyContinue | Where-Object SessionId -ne 0 | Select-Object -First 1
        if ($p) { break }
    }
    if (-not $p) { throw 'Okilum did not start in the console session within 30 s' }
    Start-Sleep 8   # window and first layout of the note
    if ($Trace) {
        # a big note keeps drawing while it loads: wait until the trace stops growing
        $last = -1; $still = 0
        foreach ($i in 1..90) {
            $size = if (Test-Path $traceFile) { (Get-Item $traceFile).Length } else { 0 }
            if ($size -eq $last -and $size -gt 0) { $still++ } else { $still = 0 }
            if ($still -ge 3) { break }
            $last = $size; Start-Sleep 1
        }
    }
    $gate = Wait-Quiet
    "rep ${rep}: guest quiet=$($gate.quiet) busy=$($gate.busy)%"

    # Wheel helper: runs in the console session, where the window and the cursor are.
@"
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class W {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, int d, UIntPtr e);
  public struct RECT { public int L, T, R, B; }
}
'@
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
function Shot(`$path) {
  `$b = [Windows.Forms.SystemInformation]::VirtualScreen
  `$bmp = New-Object Drawing.Bitmap `$b.Width, `$b.Height
  [Drawing.Graphics]::FromImage(`$bmp).CopyFromScreen(`$b.Left, `$b.Top, 0, 0, `$bmp.Size)
  `$bmp.Save(`$path, [Drawing.Imaging.ImageFormat]::Png); `$bmp.Dispose()
}
`$h = (Get-Process -Id $($p.Id)).MainWindowHandle
if (`$h -eq 0) { 'no window' | Set-Content $dir\wheel-error.txt; exit 1 }
[W]::SetForegroundWindow(`$h) | Out-Null
`$r = New-Object W+RECT; [W]::GetWindowRect(`$h, [ref]`$r) | Out-Null
[W]::SetCursorPos([int]((`$r.L + `$r.R) / 2), [int]((`$r.T + `$r.B) / 2)) | Out-Null
Start-Sleep -Milliseconds 300; Shot $dir\before.png
[DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() | Set-Content $dir\wheel-start.txt
`$sw = [Diagnostics.Stopwatch]::StartNew(); `$n = 0
if ($WheelHz -gt 0) {
  `$step = 1000.0 / $WheelHz
  while (`$sw.Elapsed.TotalSeconds -lt $Seconds) {
    if (`$sw.Elapsed.TotalMilliseconds -ge `$n * `$step) { [W]::mouse_event(0x0800, 0, 0, -120, [UIntPtr]::Zero); `$n++ } else { Start-Sleep -Milliseconds 1 }
  }
} else {
  while (`$sw.Elapsed.TotalSeconds -lt $Seconds) { [W]::mouse_event(0x0800, 0, 0, -120, [UIntPtr]::Zero); `$n++; Start-Sleep -Milliseconds 16 }
}
"`$n" | Set-Content $dir\wheel-events.txt
[DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() | Set-Content $dir\wheel-end.txt
Start-Sleep -Milliseconds 400; Shot $dir\after.png
'done' | Set-Content $dir\wheel-done.txt
"@ | Set-Content C:\qa\bench-wheel.ps1

    $etl = Join-Path $dir 'dxgi.etl'
    logman stop okilum-bench -ets 2>$null | Out-Null
    logman create trace okilum-bench -p Microsoft-Windows-DXGI 0xFFFFFFFF 5 -o $etl -ets | Out-Null
    if ($LASTEXITCODE) { throw 'logman could not start the DXGI trace' }
    $cpu0 = Get-ThreadCpu $p.Id; $wall0 = Get-Date
    & C:\qa\run-gui.ps1 -Program powershell.exe -Arguments '-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\qa\bench-wheel.ps1' | Out-Null
    if ($rep -eq 1) { & C:\qa\gpu-probe.ps1 -Process okilum -Seconds ([Math]::Min(5, $Seconds)) | Set-Content (Join-Path $root 'gpu-probe.txt') }
    foreach ($i in 1..($Seconds + 30)) { if (Test-Path (Join-Path $dir 'wheel-done.txt')) { break }; Start-Sleep 1 }
    $cpu1 = Get-ThreadCpu $p.Id; $wall1 = Get-Date
    logman stop okilum-bench -ets | Out-Null
    Get-Process okilum -ErrorAction SilentlyContinue | Stop-Process -Force
    # Threads by CPU spent while the wheel turned (the earliest-started thread is the main one).
    $wallMs = ($wall1 - $wall0).TotalMilliseconds
    $order = @($cpu1.Keys | Sort-Object { $cpu1[$_].start })
    $busy = foreach ($id in $cpu1.Keys) {
        $before = if ($cpu0.ContainsKey($id)) { $cpu0[$id].cpu } else { 0 }
        [pscustomobject]@{ id = $id; ms = [math]::Round($cpu1[$id].cpu - $before, 0); main = ($id -eq $order[0]) }
    }
    $total = ($busy | Measure-Object ms -Sum).Sum
    (@("wall_ms=$([int]$wallMs) process_cpu_ms=$total process_cpu_pct_of_one_core=$([math]::Round(100 * $total / $wallMs, 1))") +
        ($busy | Sort-Object ms -Descending | Select-Object -First 6 | ForEach-Object {
            "thread=$($_.id) cpu_ms=$($_.ms) pct_of_wall=$([math]::Round(100 * $_.ms / $wallMs, 1)) main=$($_.main)" })) |
        Set-Content (Join-Path $dir 'cpu-threads.txt')
    if (-not (Test-Path (Join-Path $dir 'wheel-done.txt'))) { throw 'the wheel helper did not finish' }

    $same = (Get-FileHash (Join-Path $dir 'before.png')).Hash -eq (Get-FileHash (Join-Path $dir 'after.png')).Hash
    $from = [DateTimeOffset]::FromUnixTimeMilliseconds([int64](Get-Content (Join-Path $dir 'wheel-start.txt'))).LocalDateTime
    $to = [DateTimeOffset]::FromUnixTimeMilliseconds([int64](Get-Content (Join-Path $dir 'wheel-end.txt'))).LocalDateTime
    # Present_Start (event 42) of Okilum, only while the wheel turned.
    $starts = @(Get-WinEvent -Path $etl -Oldest | Where-Object { $_.Id -eq 42 -and $_.ProcessId -eq $p.Id } |
        ForEach-Object { $_.TimeCreated } | Where-Object { $_ -ge $from -and $_ -le $to })
    $valid = (-not $same) -and ($starts.Count -ge 10)
    $eventsFile = Join-Path $dir 'wheel-events.txt'
    $events = if (Test-Path $eventsFile) { [int](Get-Content $eventsFile) } else { -1 }
    $wheelRate = if ($events -gt 0) { [math]::Round($events / $Seconds, 1) } else { -1 }
    $line = "rep=$rep valid=$valid scrolled=$(-not $same) quiet=$($gate.quiet) busy=$($gate.busy) host_capped=$hostCapped dwm_p50=$dwmP50 wheel_hz=$wheelRate presents=$($starts.Count)"
    if ($starts.Count -ge 2) {
        $ms = @(for ($i = 1; $i -lt $starts.Count; $i++) { ($starts[$i] - $starts[$i - 1]).TotalMilliseconds }) | Sort-Object
        $pct = { param($q) $ms[[Math]::Min($ms.Count - 1, [int][Math]::Floor($q * $ms.Count))] }
        $span = ($starts[-1] - $starts[0]).TotalSeconds
        $line += (' fps={0:N1} median={1:N2} p95={2:N2} p99={3:N2} max={4:N2} over33={5}' -f ($ms.Count / $span),
            (& $pct 0.5), (& $pct 0.95), (& $pct 0.99), $ms[-1], @($ms | Where-Object { $_ -gt 33.4 }).Count)
    }
    $line | Tee-Object (Join-Path $dir 'summary.txt')
    Remove-Item $etl -ErrorAction SilentlyContinue   # large; the summary has what is needed
}
Get-ChildItem $root -Recurse -Filter summary.txt | Sort-Object FullName | ForEach-Object { "$($_.Directory.Name): $(Get-Content $_.FullName)" } | Tee-Object (Join-Path $root 'summary-all.txt')
