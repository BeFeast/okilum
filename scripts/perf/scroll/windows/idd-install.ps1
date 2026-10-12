# Give a Windows QA VM whose GPU has no monitor a virtual one at 60 Hz (#1290).
# VM 151 (Iris Xe passed through, no display attached): DWM sees 0 monitors with an EDID, its
# timing query fails and DwmFlush() returns every 79 ms, which caps every app at 12.5 fps. An
# indirect display (IddCx) monitor gives DWM a 60 Hz clock again.
# Driver: VirtualDrivers/Virtual-Display-Driver "Signed-Driver-v24.12.24-x64.zip" (release 25.5.2;
# MttVDD.dll, MttVDD.inf, mttvdd.cat, signed by SignPath Foundation via GlobalSign), copied to
# -Dir. The device is created the way devcon does (SetupAPI: root device + UpdateDriverForPlugAndPlayDevices),
# not with the vendor's installer. Run over SSH as qa (an administrator), take a VM snapshot first:
#   powershell -ExecutionPolicy Bypass -File C:\qa\idd-install.ps1 [-Dir C:\qa\idd] [-Gpu 'Intel(R) Iris(R) Xe Graphics']
param([string]$Dir = 'C:\qa\idd', [string]$Gpu = '', [switch]$CheckOnly)
$ErrorActionPreference = 'Stop'
foreach ($f in 'MttVDD.dll', 'MttVDD.inf', 'mttvdd.cat', 'vdd_settings.xml') {
    if (-not (Test-Path (Join-Path $Dir $f))) { throw "missing $f in $Dir" }
}
'== signatures (must be Valid, no prompt can appear then)'
foreach ($f in 'mttvdd.cat', 'MttVDD.dll') {
    $sig = Get-AuthenticodeSignature (Join-Path $Dir $f)
    '{0}: {1}  signer: {2}' -f $f, $sig.Status, $sig.SignerCertificate.Subject
    if ($sig.Status -ne 'Valid') { throw "$f is not validly signed on this machine: $($sig.StatusMessage)" }
}
# Windows installs a non-Microsoft driver package without a prompt only when its publisher is in
# TrustedPublisher (error 0xE0000242 otherwise): add this one signer certificate, as the vendor's
# installer does. This changes the guest's trust store, which is why the VM is snapshotted first.
'== publisher trust'
$signer = (Get-AuthenticodeSignature (Join-Path $Dir 'mttvdd.cat')).SignerCertificate
$store = 'Cert:\LocalMachine\TrustedPublisher'
if (-not $CheckOnly) {
    if (-not (Get-ChildItem $store | Where-Object Thumbprint -eq $signer.Thumbprint)) {
        $cer = Join-Path $env:TEMP 'vdd-signer.cer'
        Export-Certificate -Cert $signer -FilePath $cer | Out-Null
        Import-Certificate -FilePath $cer -CertStoreLocation $store | Out-Null
        "added to TrustedPublisher: $($signer.Subject) $($signer.Thumbprint)"
    } else { 'publisher already trusted' }
    # Nodes an earlier failed attempt left behind: root display devices that never got a driver.
    Get-PnpDevice -Class Display -ErrorAction SilentlyContinue | Where-Object { $_.InstanceId -like 'ROOT\DISPLAY\*' -and -not $_.FriendlyName } | ForEach-Object {
        pnputil /remove-device $_.InstanceId | Out-Null
        "removed leftover device node $($_.InstanceId)"
    }
} else { "would trust $($signer.Subject) $($signer.Thumbprint)" }
if (-not $Gpu) { $Gpu = (Get-CimInstance Win32_VideoController | Where-Object { $_.Name -notmatch 'Virtual|Basic|Remote' } | Select-Object -First 1).Name }
"render adapter: $Gpu"
if ($CheckOnly) { 'check only: nothing installed'; return }

# The driver reads its settings from C:\VirtualDisplayDriver when it starts.
New-Item 'C:\VirtualDisplayDriver' -ItemType Directory -Force | Out-Null
@"
<?xml version='1.0' encoding='utf-8'?>
<vdd_settings>
    <monitors><count>1</count></monitors>
    <gpu><friendlyname>$Gpu</friendlyname></gpu>
    <global><g_refresh_rate>60</g_refresh_rate></global>
    <resolutions>
        <resolution><width>1920</width><height>1080</height><refresh_rate>60</refresh_rate></resolution>
    </resolutions>
    <options>
        <CustomEdid>false</CustomEdid><PreventSpoof>false</PreventSpoof><EdidCeaOverride>false</EdidCeaOverride>
        <HardwareCursor>true</HardwareCursor><SDR10bit>false</SDR10bit><HDRPlus>false</HDRPlus>
        <logging>true</logging><debuglogging>false</debuglogging>
    </options>
</vdd_settings>
"@ | Set-Content 'C:\VirtualDisplayDriver\vdd_settings.xml' -Encoding utf8

Add-Type @'
using System; using System.ComponentModel; using System.Runtime.InteropServices; using System.Text;
public static class Idd {
  [StructLayout(LayoutKind.Sequential)] struct SP_DEVINFO_DATA { public int cbSize; public Guid ClassGuid; public int DevInst; public IntPtr Reserved; }
  [DllImport("setupapi.dll", SetLastError = true)] static extern IntPtr SetupDiCreateDeviceInfoList(ref Guid cls, IntPtr parent);
  [DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)] static extern bool SetupDiCreateDeviceInfoW(IntPtr set, string name, ref Guid cls, string desc, IntPtr parent, int flags, ref SP_DEVINFO_DATA data);
  [DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)] static extern bool SetupDiSetDeviceRegistryPropertyW(IntPtr set, ref SP_DEVINFO_DATA data, int prop, byte[] buf, int size);
  [DllImport("setupapi.dll", SetLastError = true)] static extern bool SetupDiCallClassInstaller(int func, IntPtr set, ref SP_DEVINFO_DATA data);
  [DllImport("setupapi.dll", SetLastError = true)] static extern bool SetupDiDestroyDeviceInfoList(IntPtr set);
  [DllImport("newdev.dll", SetLastError = true, CharSet = CharSet.Unicode)] static extern bool UpdateDriverForPlugAndPlayDevicesW(IntPtr parent, string hwid, string inf, int flags, out bool reboot);
  public static string Install(string inf, string hwid) {
    Guid cls = new Guid("4D36E968-E325-11CE-BFC1-08002BE10318"); // Display
    IntPtr set = SetupDiCreateDeviceInfoList(ref cls, IntPtr.Zero);
    if (set == (IntPtr)(-1)) throw new Win32Exception(Marshal.GetLastWin32Error(), "SetupDiCreateDeviceInfoList");
    try {
      SP_DEVINFO_DATA data = new SP_DEVINFO_DATA(); data.cbSize = Marshal.SizeOf(typeof(SP_DEVINFO_DATA));
      if (!SetupDiCreateDeviceInfoW(set, "Display", ref cls, null, IntPtr.Zero, 1, ref data)) throw new Win32Exception(Marshal.GetLastWin32Error(), "SetupDiCreateDeviceInfo");
      byte[] id = Encoding.Unicode.GetBytes(hwid + "\0\0");
      if (!SetupDiSetDeviceRegistryPropertyW(set, ref data, 1, id, id.Length)) throw new Win32Exception(Marshal.GetLastWin32Error(), "SetupDiSetDeviceRegistryProperty(HARDWAREID)");
      if (!SetupDiCallClassInstaller(0x19, set, ref data)) throw new Win32Exception(Marshal.GetLastWin32Error(), "SetupDiCallClassInstaller(DIF_REGISTERDEVICE)");
    } finally { SetupDiDestroyDeviceInfoList(set); }
    bool reboot;
    if (!UpdateDriverForPlugAndPlayDevicesW(IntPtr.Zero, hwid, inf, 1, out reboot)) throw new Win32Exception(Marshal.GetLastWin32Error(), "UpdateDriverForPlugAndPlayDevices");
    return "driver installed on " + hwid + "; reboot required: " + reboot;
  }
}
'@
'== install'
[Idd]::Install((Join-Path $Dir 'MttVDD.inf'), 'Root\MttVDD')
Start-Sleep 8
'== what Windows sees now'
Get-CimInstance Win32_VideoController | ForEach-Object { '{0} | {1}x{2} @ {3} Hz | status {4}' -f $_.Name, $_.CurrentHorizontalResolution, $_.CurrentVerticalResolution, $_.CurrentRefreshRate, $_.Status }
'monitors with EDID: ' + @(Get-CimInstance -Namespace root\wmi -ClassName WmiMonitorID -ErrorAction SilentlyContinue).Count
Get-PnpDevice -Class Monitor -ErrorAction SilentlyContinue | ForEach-Object { 'monitor device: {0} [{1}]' -f $_.FriendlyName, $_.Status }
Get-PnpDevice -Class Display -ErrorAction SilentlyContinue | ForEach-Object { 'display device: {0} [{1}]' -f $_.FriendlyName, $_.Status }
