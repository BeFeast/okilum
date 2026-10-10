# Okilum Windows QA VM provisioning (#1113): runs once at qa's first logon, from the
# provision CD, then powers the VM off so the owner's command can snapshot it as `clean`.
param([Parameter(Mandatory)][string]$Media)
$ErrorActionPreference = 'Stop'
Start-Transcript -Path 'C:\okilum-qa-provision.transcript.txt' -Append | Out-Null
function Step($text) { Write-Output "[$(Get-Date -Format s)] $text" }

function Find-Drive($relative) {
    foreach ($drive in Get-PSDrive -PSProvider FileSystem) {
        $path = Join-Path $drive.Root $relative
        if (Test-Path $path) { return $path }
    }
    throw "not found on any drive: $relative"
}

Step 'QEMU guest agent (reports the IP to Proxmox, clean shutdown)'
$agent = Find-Drive 'guest-agent\qemu-ga-x86_64.msi'
Start-Process msiexec.exe -Wait -ArgumentList '/i', "`"$agent`"", '/qn', '/norestart'
Set-Service QEMU-GA -StartupType Automatic
Start-Service QEMU-GA

Step 'Private network, Remote Desktop'
Get-NetConnectionProfile | Set-NetConnectionProfile -NetworkCategory Private
Set-ItemProperty 'HKLM:\System\CurrentControlSet\Control\Terminal Server' -Name fDenyTSConnections -Value 0
Enable-NetFirewallRule -DisplayGroup 'Remote Desktop'

Step 'OpenSSH Server, key-only for god@maestro, Windows PowerShell as the shell'
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0 | Out-Null
Set-Service sshd -StartupType Automatic
Start-Service sshd
# qa is an administrator: OpenSSH reads administrators_authorized_keys, owned by SYSTEM/Administrators only.
$keys = 'C:\ProgramData\ssh\administrators_authorized_keys'
Copy-Item (Join-Path $Media 'authorized_keys') $keys -Force
icacls.exe $keys /inheritance:r /grant 'Administrators:F' /grant 'SYSTEM:F' | Out-Null
$config = 'C:\ProgramData\ssh\sshd_config'
(Get-Content $config) -replace '^#?PasswordAuthentication .*', 'PasswordAuthentication no' | Set-Content $config
New-ItemProperty 'HKLM:\SOFTWARE\OpenSSH' -Name DefaultShell -PropertyType String -Force `
    -Value 'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe' | Out-Null
Restart-Service sshd
if (-not (Get-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -ErrorAction SilentlyContinue)) {
    New-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -DisplayName 'OpenSSH Server (sshd)' -Enabled True `
        -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22 | Out-Null
}

Step 'Permanent autologon of qa: the GUI session QA drives is always there'
# The password comes from the answer CD this boot used; it is not copied anywhere else.
[xml]$answer = Get-Content (Find-Drive 'Autounattend.xml')
$password = ($answer.unattend.settings | Where-Object pass -eq 'oobeSystem').component |
    Where-Object name -eq 'Microsoft-Windows-Shell-Setup' |
    ForEach-Object { $_.AutoLogon.Password.Value } | Select-Object -First 1
$winlogon = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon'
Set-ItemProperty $winlogon -Name AutoAdminLogon -Value '1'
Set-ItemProperty $winlogon -Name DefaultUserName -Value 'qa'
Set-ItemProperty $winlogon -Name DefaultDomainName -Value $env:COMPUTERNAME
Set-ItemProperty $winlogon -Name DefaultPassword -Value $password
Remove-ItemProperty $winlogon -Name AutoLogonCount -ErrorAction SilentlyContinue

Step 'No sleep, no lock screen, no reboot for updates while qa is logged in'
powercfg.exe /change standby-timeout-ac 0
powercfg.exe /change monitor-timeout-ac 0
powercfg.exe /change hibernate-timeout-ac 0
New-Item 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\Personalization' -Force | Out-Null
Set-ItemProperty 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\Personalization' -Name NoLockScreen -Value 1 -Type DWord
Set-ItemProperty 'HKCU:\Control Panel\Desktop' -Name ScreenSaveActive -Value '0'
New-Item 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU' -Force | Out-Null
Set-ItemProperty 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU' -Name NoAutoRebootWithLoggedOnUsers -Value 1 -Type DWord

Step 'QA helpers'
New-Item C:\qa -ItemType Directory -Force | Out-Null
Copy-Item (Join-Path $Media 'run-gui.ps1') C:\qa\run-gui.ps1 -Force
"provisioned $(Get-Date -Format s); Windows $((Get-CimInstance Win32_OperatingSystem).Caption) $((Get-CimInstance Win32_OperatingSystem).Version)" |
    Set-Content C:\qa\provisioned.txt

Step 'Done: power off for the clean snapshot'
Stop-Transcript | Out-Null
shutdown.exe /s /t 20 /c 'Okilum QA provisioning finished; snapshot clean follows'
