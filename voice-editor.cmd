@echo off
rem Voice editor for Jarvis recordings: opens a local page in the browser.
rem Close this window to stop the editor.
cd /d "%~dp0"
py -3.12 tools\voice\pack_editor.py
if errorlevel 1 pause
