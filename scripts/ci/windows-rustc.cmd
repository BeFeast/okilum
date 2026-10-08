@echo off
python "%~dp0..\windows-rustc.py" %*
exit /b %ERRORLEVEL%
