# Start a program in qa's interactive desktop from an SSH session (#1113).
# SSH sessions have no desktop; a one-shot scheduled task with an interactive logon runs
# in the console session that autologon keeps open, like QA on bragi.
#   powershell -File C:\qa\run-gui.ps1 -Program 'C:\path\app.exe' [-Arguments '...']
param([Parameter(Mandatory)][string]$Program, [string]$Arguments = '')
$name = "okilum-qa-gui-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
$action = if ($Arguments) { New-ScheduledTaskAction -Execute $Program -Argument $Arguments } else { New-ScheduledTaskAction -Execute $Program }
$principal = New-ScheduledTaskPrincipal -UserId 'qa' -LogonType Interactive -RunLevel Limited
Register-ScheduledTask -TaskName $name -Action $action -Principal $principal | Out-Null
Start-ScheduledTask -TaskName $name
Start-Sleep -Seconds 2
Unregister-ScheduledTask -TaskName $name -Confirm:$false
"started $Program in qa's desktop"
