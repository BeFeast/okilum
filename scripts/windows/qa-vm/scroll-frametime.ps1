# Frame times of Okilum scrolling a large note, inside a Windows QA VM (#1264).
# Run over SSH as qa (an administrator), with Okilum installed for qa:
#   powershell -File C:\qa\scroll-frametime.ps1 [-Seconds 10] [-Out C:\qa\bench]
# Okilum starts in qa's console session (run-gui.ps1) on a generated ~30k-line note, a
# console-session helper turns the mouse wheel over its window every 16 ms, an ETW trace of
# Microsoft-Windows-DXGI records every Present of okilum.exe, and gpu-probe.ps1 names the
# adapter doing the work. Frame time = interval between Present_Start events (PresentMon's
# MsBetweenPresents); PresentMon itself records nothing on the Basic Display driver of the
# no-GPU control VM, which emits no flip events, so it is not used.
# The numbers are host-specific: compare only runs from the same VM (AGENTS: one machine).
param([int]$Seconds = 10, [string]$Out = 'C:\qa\bench')
$ErrorActionPreference = 'Stop'
New-Item $Out -ItemType Directory -Force | Out-Null
Get-Process okilum -ErrorAction SilentlyContinue | Stop-Process -Force

$exe = Get-ChildItem 'C:\Users\qa\AppData\Local' -Filter okilum.exe -Recurse -Depth 3 -ErrorAction SilentlyContinue |
    Where-Object FullName -like '*\current\*' | Select-Object -First 1 -ExpandProperty FullName
if (-not $exe) { throw 'Okilum is not installed for qa' }

$vault = 'C:\qa\bench-vault'
New-Item $vault -ItemType Directory -Force | Out-Null
$note = Join-Path $vault 'big.md'
if (-not (Test-Path $note)) {
    $sb = [System.Text.StringBuilder]::new()
    foreach ($i in 1..1500) {
        [void]$sb.AppendLine("## Section $i").AppendLine()
        [void]$sb.AppendLine("Paragraph $i with **bold**, *italic*, ``code`` and a [[link $i]]. " * 3).AppendLine()
        [void]$sb.AppendLine("- item one of $i").AppendLine("- item two with a longer line of text $i").AppendLine("- [ ] task $i").AppendLine()
        [void]$sb.AppendLine('```rust').AppendLine("fn section_$i() -> u32 { $i }").AppendLine('```').AppendLine()
        [void]$sb.AppendLine("| a | b |").AppendLine("|---|---|").AppendLine("| $i | $($i * 2) |").AppendLine()
    }
    [IO.File]::WriteAllText($note, $sb.ToString())
}
"note: $note, $((Get-Content $note).Count) lines"

# Own state directory: never the qa profile's real Okilum state.
$state = Join-Path $Out 'state'
Remove-Item $state -Recurse -Force -ErrorAction SilentlyContinue
"@echo off`r`nset OKILUM_STATE_DIR=$state`r`nstart `"`" `"$exe`" `"$note`"`r`n" |
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

# Wheel helper: runs in the console session, where the window and the cursor are. It saves
# the screen before and after scrolling: identical shots mean the wheel moved nothing and the
# frame times are of an idle window.
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
if (`$h -eq 0) { 'no window' | Set-Content $Out\wheel-error.txt; exit 1 }
[W]::SetForegroundWindow(`$h) | Out-Null
`$r = New-Object W+RECT; [W]::GetWindowRect(`$h, [ref]`$r) | Out-Null
[W]::SetCursorPos([int]((`$r.L + `$r.R) / 2), [int]((`$r.T + `$r.B) / 2)) | Out-Null
Start-Sleep -Milliseconds 300; Shot $Out\before.png
`$end = (Get-Date).AddSeconds($Seconds)
while ((Get-Date) -lt `$end) { [W]::mouse_event(0x0800, 0, 0, -120, [UIntPtr]::Zero); Start-Sleep -Milliseconds 16 }
Shot $Out\after.png
'done' | Set-Content $Out\wheel-done.txt
"@ | Set-Content C:\qa\bench-wheel.ps1

$etl = Join-Path $Out 'dxgi.etl'
Remove-Item $etl, (Join-Path $Out 'wheel-*.txt'), (Join-Path $Out '*.png') -ErrorAction SilentlyContinue
logman stop okilum-bench -ets 2>$null | Out-Null
logman create trace okilum-bench -p Microsoft-Windows-DXGI 0xFFFFFFFF 5 -o $etl -ets | Out-Null
if ($LASTEXITCODE) { throw 'logman could not start the DXGI trace' }
& C:\qa\run-gui.ps1 -Program powershell.exe -Arguments '-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\qa\bench-wheel.ps1' | Out-Null
& C:\qa\gpu-probe.ps1 -Process okilum -Seconds ([Math]::Min(5, $Seconds)) | Tee-Object (Join-Path $Out 'gpu-probe.txt')
foreach ($i in 1..($Seconds + 30)) { if (Test-Path (Join-Path $Out 'wheel-done.txt')) { break }; Start-Sleep 1 }
if (-not (Test-Path (Join-Path $Out 'wheel-done.txt'))) { logman stop okilum-bench -ets | Out-Null; throw 'the wheel helper did not finish' }
logman stop okilum-bench -ets | Out-Null
Get-Process okilum -ErrorAction SilentlyContinue | Stop-Process -Force
$same = (Get-FileHash (Join-Path $Out 'before.png')).Hash -eq (Get-FileHash (Join-Path $Out 'after.png')).Hash
if ($same) { throw 'before.png equals after.png: the note did not scroll' }

# Present_Start (event 42) of Okilum, only while the wheel turned: between the two shots.
$from = (Get-Item (Join-Path $Out 'before.png')).LastWriteTime
$to = (Get-Item (Join-Path $Out 'after.png')).LastWriteTime
$starts = @(Get-WinEvent -Path $etl -Oldest | Where-Object { $_.Id -eq 42 -and $_.ProcessId -eq $p.Id } |
    ForEach-Object { $_.TimeCreated } | Where-Object { $_ -ge $from -and $_ -le $to })
if ($starts.Count -lt 10) { throw "only $($starts.Count) presents while scrolling: Okilum did not draw" }
$ms = @(for ($i = 1; $i -lt $starts.Count; $i++) { ($starts[$i] - $starts[$i - 1]).TotalMilliseconds }) | Sort-Object
function Pct($q) { $ms[[Math]::Min($ms.Count - 1, [int][Math]::Floor($q * $ms.Count))] }
$span = ($starts[-1] - $starts[0]).TotalSeconds
'== frame times (ms between presents): frames {0} in {1:N1} s ({2:N1} fps), median {3:N2}, p95 {4:N2}, p99 {5:N2}, max {6:N2}, >33ms {7}' -f `
    $ms.Count, $span, ($ms.Count / $span), (Pct 0.5), (Pct 0.95), (Pct 0.99), $ms[-1], @($ms | Where-Object { $_ -gt 33.4 }).Count |
    Tee-Object (Join-Path $Out 'summary.txt')
