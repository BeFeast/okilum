# Which adapter does Okilum render on (#1264)? Run over SSH as qa (an administrator):
#   powershell -File C:\qa\gpu-probe.ps1 [-Process okilum] [-Seconds 5]
# 1. Display adapters with their driver and device-manager error (43 = the driver refused).
# 2. DXGI adapters (what gpui enumerates) with LUID, vendor and the software flag.
# 3. The Process's 3D engine use per adapter LUID from the per-process GPU Engine counters.
# WARP does get GPU Engine time (on the Microsoft Basic Render LUID, VM 150 shows hundreds of
# %-samples), so the verdict is the adapter that carries the time: hardware means a DXGI
# adapter without DXGI_ADAPTER_FLAG_SOFTWARE and not Microsoft's vendor id 0x1414.
param([string]$Process = 'okilum', [int]$Seconds = 5)
$ErrorActionPreference = 'Stop'

'== display adapters'
Get-CimInstance Win32_VideoController | ForEach-Object {
    $dev = Get-CimInstance Win32_PnPEntity -Filter "PNPDeviceID='$($_.PNPDeviceID -replace '\\', '\\')'"
    '{0} | driver {1} | status {2} | error {3} | {4}' -f $_.Name, $_.DriverVersion, $_.Status, $dev.ConfigManagerErrorCode, $_.PNPDeviceID
}

Add-Type @'
using System; using System.Runtime.InteropServices; using System.Collections.Generic;
public static class Dxgi {
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
  public struct DESC1 {
    [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string Description;
    public uint VendorId, DeviceId, SubSysId, Revision;
    public UIntPtr DedicatedVideoMemory, DedicatedSystemMemory, SharedSystemMemory;
    public uint LuidLow; public int LuidHigh; public uint Flags;
  }
  [ComImport, Guid("29038f61-3839-4626-91fd-086879011a05"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  interface IAdapter1 { void a0(); void a1(); void a2(); void a3(); void a4(); void a5(); void a6();
    [PreserveSig] int GetDesc1(out DESC1 d); }
  [ComImport, Guid("770aae78-f26f-4dba-a829-253c83d1b387"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  interface IFactory1 { void f0(); void f1(); void f2(); void f3(); void f4(); void f5(); void f6(); void f7(); void f8();
    [PreserveSig] int EnumAdapters1(uint i, out IAdapter1 a); }
  [DllImport("dxgi.dll")] static extern int CreateDXGIFactory1(ref Guid riid, [MarshalAs(UnmanagedType.IUnknown)] out object f);
  public static List<DESC1> Adapters() {
    var g = new Guid("770aae78-f26f-4dba-a829-253c83d1b387"); object o;
    Marshal.ThrowExceptionForHR(CreateDXGIFactory1(ref g, out o));
    var f = (IFactory1)o; var list = new List<DESC1>(); IAdapter1 a;
    for (uint i = 0; f.EnumAdapters1(i, out a) == 0; i++) { DESC1 d; a.GetDesc1(out d); list.Add(d); }
    return list;
  }
}
'@
'== DXGI adapters (gpui uses the first that creates a D3D11 device)'
$byLuid = @{}
foreach ($d in [Dxgi]::Adapters()) {
    $luid = 'luid_0x{0:x8}_0x{1:x8}' -f $d.LuidHigh, $d.LuidLow
    $hw = -not ($d.Flags -band 2) -and $d.VendorId -ne 0x1414
    $byLuid[$luid] = [pscustomobject]@{ Name = $d.Description; Hardware = $hw }
    '{0} | vendor 0x{1:x4} device 0x{2:x4} | flags {3} | {4} | {5}' -f $d.Description, $d.VendorId, $d.DeviceId, $d.Flags, $luid, ($(if ($hw) { 'hardware' } else { 'software' }))
}

$procs = @(Get-Process -Name $Process -ErrorAction SilentlyContinue)
if (-not $procs) { "== no running process named $Process"; exit 2 }
$pids = $procs.Id
"== 3D engine utilisation of $Process (pid $($pids -join ',')) over $Seconds s, per adapter"
$samples = (Get-Counter '\GPU Engine(*engtype_3D)\Utilization Percentage' -SampleInterval 1 -MaxSamples $Seconds).CounterSamples
$mine = $samples | Where-Object { $i = $_.InstanceName; $pids | Where-Object { $i -like "pid_$($_)_*" } }
$hwTime = 0
$mine | Group-Object { ($_.InstanceName -split '_phys_')[0] -replace '^pid_\d+_', '' } | ForEach-Object {
    $sum = ($_.Group | Measure-Object CookedValue -Sum).Sum
    $a = $byLuid[$_.Name]
    if ($a -and $a.Hardware) { $hwTime += $sum }
    '{0} ({1}): {2:N1} %-samples' -f $_.Name, $(if ($a) { $a.Name } else { 'not a DXGI adapter' }), $sum
}
if ($hwTime -le 0) { "verdict: $Process does not render on a hardware adapter"; exit 1 }
"verdict: $Process renders on a hardware adapter"
