# How fast does DWM pace a frame on this host, without Okilum? (#1290)
# gpui on Windows draws on WM_PAINT, which its VSyncProvider thread invalidates after every
# DwmFlush() (gpui-pre-windows vsync.rs); if DwmFlush itself returns every 80 ms, no app on this
# host draws faster. This times DwmFlush in a plain process, reads the refresh rate DWM reports
# (what gpui reads for its fallback sleep) and names the display, in qa's console session:
#   powershell -File C:\qa\run-gui.ps1 -Program powershell.exe -Arguments '-NoProfile -ExecutionPolicy Bypass -File C:\qa\dwm-flush.ps1'
# and the result is C:\qa\bench\dwm-flush.txt. Run it with Okilum closed, and again with it open and idle.
param([int]$Count = 120, [string]$Out = 'C:\qa\bench\dwm-flush.txt', [string]$Label = 'idle')
$ErrorActionPreference = 'Continue'
New-Item (Split-Path $Out) -ItemType Directory -Force | Out-Null
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class Dwm {
  [StructLayout(LayoutKind.Sequential)] public struct RATIO { public uint Numerator; public uint Denominator; }
  [StructLayout(LayoutKind.Sequential)] public struct TIMING {
    public uint cbSize; public RATIO rateRefresh; public ulong qpcRefreshPeriod; public RATIO rateCompose;
    public ulong qpcVBlank, cRefresh, qpcCompose, cFrame, cRefreshFrame, cRefreshConfirmed;
    public uint cFlipsOutstanding;
    public ulong cFrameCurrent, cFramesAvailable, cFramesReceived, cFramesDisplayed, cFramesDropped, cFramesMissed,
      cRefreshNextDisplayed, cRefreshNextPresented, cRefreshesDisplayed, cRefreshesPresented, cRefreshStarted,
      cPixelsReceived, cPixelsDrawn, cBuffersEmpty;
  }
  [DllImport("dwmapi.dll")] public static extern int DwmFlush();
  [DllImport("dwmapi.dll")] public static extern int DwmGetCompositionTimingInfo(IntPtr hwnd, ref TIMING info);
}
'@
$lines = New-Object System.Collections.Generic.List[string]
$lines.Add("label: $Label   date: $((Get-Date).ToString('o'))   session: $([System.Diagnostics.Process]::GetCurrentProcess().SessionId)")
$ti = New-Object Dwm+TIMING
$ti.cbSize = [Runtime.InteropServices.Marshal]::SizeOf($ti)
$hr = [Dwm]::DwmGetCompositionTimingInfo([IntPtr]::Zero, [ref]$ti)
$qpf = [Diagnostics.Stopwatch]::Frequency
$lines.Add(("DwmGetCompositionTimingInfo hr=0x{0:x} size={1}: refresh {2}/{3} = {4:N2} Hz, refresh period {5:N3} ms, compose {6}/{7}" -f $hr, $ti.cbSize,
    $ti.rateRefresh.Numerator, $ti.rateRefresh.Denominator,
    $(if ($ti.rateRefresh.Denominator) { $ti.rateRefresh.Numerator / $ti.rateRefresh.Denominator } else { 0 }),
    ($ti.qpcRefreshPeriod * 1000.0 / $qpf), $ti.rateCompose.Numerator, $ti.rateCompose.Denominator))
Get-CimInstance Win32_VideoController | ForEach-Object {
    $lines.Add("video: $($_.Name) | $($_.CurrentHorizontalResolution)x$($_.CurrentVerticalResolution) @ $($_.CurrentRefreshRate) Hz | driver $($_.DriverVersion) | status $($_.Status)")
}
$monitors = @(Get-CimInstance -Namespace root\wmi -ClassName WmiMonitorID -ErrorAction SilentlyContinue)
$lines.Add("monitors with EDID: $($monitors.Count)")
foreach ($m in $monitors) { $lines.Add("  monitor: " + (($m.UserFriendlyName | Where-Object { $_ }) | ForEach-Object { [char]$_ }) -join '') }
$ms = New-Object System.Collections.Generic.List[double]
$sw = [Diagnostics.Stopwatch]::new()
foreach ($i in 1..$Count) {
    $sw.Restart()
    [void][Dwm]::DwmFlush()
    $ms.Add($sw.Elapsed.TotalMilliseconds)
}
$sorted = $ms | Sort-Object
$lines.Add(('DwmFlush x{0}: mean {1:N2} ms, p50 {2:N2}, p95 {3:N2}, min {4:N2}, max {5:N2}  ({6:N1} flushes/s)' -f $Count,
    ($ms | Measure-Object -Average).Average, $sorted[[int]($Count * 0.5)], $sorted[[int]($Count * 0.95)], $sorted[0], $sorted[-1],
    ($Count / (($ms | Measure-Object -Sum).Sum / 1000))))
$lines.Add('first 12 waits (ms): ' + (($ms | Select-Object -First 12 | ForEach-Object { '{0:N1}' -f $_ }) -join ' '))
$lines | Add-Content -Path $Out -Encoding utf8
$lines
