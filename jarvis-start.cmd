@echo off
rem Start or restart Jarvis: stop the running copy (if any), wait until it exits, start again.
rem Double-click this file or run it from any terminal.
cd /d "%~dp0"
target\release\jarvis.exe --stop >nul 2>&1
set /a waited=0
:wait
tasklist /fi "imagename eq jarvis.exe" 2>nul | find /i "jarvis.exe" >nul || goto start
if %waited% geq 15 (
    echo Jarvis did not stop in 15 s. Use "Exit" in the tray icon, then run this file again.
    pause
    exit /b 1
)
timeout /t 1 /nobreak >nul
set /a waited+=1
goto wait
:start
rem jarvisw.exe starts jarvis.exe without a console window.
start "" target\release\jarvisw.exe
