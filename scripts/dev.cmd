@echo off
REM Fast-reload dev launcher. Double-click it, or run it from any directory and
REM any shell (cmd, PowerShell, Git Bash) - %~dp0 is this file's own folder, so
REM there is no relative path to get wrong and no execution policy to trip over.
REM
REM   scripts\dev.cmd              side by side with the installed app
REM   scripts\dev.cmd -Takeover    close the installed app, dev takes :42800
REM   scripts\dev.cmd -Stop        stop the dev stack
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0dev-windows.ps1" %*
