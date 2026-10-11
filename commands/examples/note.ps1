# Пример скрипта для run_script: дописывает строку в notes.txt рядом с собой.
# Аргумент — значение слота mood (great / good / meh).
param([string]$Mood = 'unknown')

$ErrorActionPreference = 'Stop'
$file = Join-Path $PSScriptRoot 'notes.txt'
$line = '{0:yyyy-MM-dd HH:mm}  {1}' -f (Get-Date), $Mood
Add-Content -LiteralPath $file -Value $line -Encoding UTF8
Write-Output "записано: $line"
