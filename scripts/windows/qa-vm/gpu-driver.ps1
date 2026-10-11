# Intel graphics driver for the GPU QA VM (#1264), after gpu-passthrough.sh. Run over SSH as qa
# (an administrator):  powershell -ExecutionPolicy Bypass -File C:\qa\gpu-driver.ps1
# Tiger Lake is on Intel's legacy branch: "Intel 11th-14th Gen Processor Graphics" 101.7092
# (intel.com download 864990). Windows Update is kept away from drivers first: during the first
# install it put Dell's 101.7088 over Intel's 101.7092, and a QA image whose driver can change
# under it gives measurements nobody can repeat. The device can still pick 101.7088 from the
# driver store as the better match for the Dell subsystem: the version is reported, not forced.
$ErrorActionPreference = 'Stop'
$url = 'https://downloadmirror.intel.com/929468/gfx_win_101.7092.exe'
$sha512 = '35a863fc2ace37e267a1602eb992e54f909dbce6122b7224ad500301bbdb2389' +
          'c38741800ffc4e389d91e1444bc96c2e26ee4283c97c4a7c7060064724385b9d'

$wu = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate'
New-Item $wu -Force | Out-Null
Set-ItemProperty $wu -Name ExcludeWUDriversInQualityUpdate -Value 1 -Type DWord
# The policy above covers Windows Update scans only; device installation searches Windows Update
# on its own when a device appears (that path still brought Dell's 101.7088 in on 2026-10-11).
Set-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\DriverSearching' -Name SearchOrderConfig -Value 0 -Type DWord

$exe = 'C:\qa\gfx_win_101.7092.exe'
if (-not (Test-Path $exe) -or (Get-FileHash $exe -Algorithm SHA512).Hash.ToLower() -ne $sha512) {
    curl.exe -fsSL -A Mozilla/5.0 -o $exe $url
}
if ((Get-FileHash $exe -Algorithm SHA512).Hash.ToLower() -ne $sha512) { throw 'driver SHA512 differs from intel.com' }
$sig = Get-AuthenticodeSignature $exe
if ($sig.Status -ne 'Valid' -or $sig.SignerCertificate.Subject -notlike 'CN=Intel Corporation,*') { throw "driver signature: $($sig.Status)" }

# Exit 0 = installed; the GUI-less installer also returns 1000 when a reboot is pending.
$p = Start-Process $exe -ArgumentList '-s' -PassThru -Wait
"installer exit $($p.ExitCode)"
Get-CimInstance Win32_VideoController | ForEach-Object {
    '{0} | driver {1} | error {2}' -f $_.Name, $_.DriverVersion, $_.ConfigManagerErrorCode
}
