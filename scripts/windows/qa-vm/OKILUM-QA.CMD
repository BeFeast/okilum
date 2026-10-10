@echo off
rem Okilum Windows QA VM first-logon provisioning (#1113). Runs once from the provision CD.
set "HERE=%~dp0"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%HERE%provision.ps1" -Media "%HERE%" >C:\okilum-qa-provision.log 2>&1
