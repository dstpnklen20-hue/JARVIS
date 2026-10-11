<#
.SYNOPSIS
    Создаёт стандартный звуковой пакет sounds/default голосом Windows.

.DESCRIPTION
    Пакет уже лежит в git — скрипт нужен, чтобы пересоздать его или собрать
    свой пакет другим голосом. Свой пакет можно сделать и вручную: папка в
    sounds/ с WAV-файлами и pack.toml (см. README, «Звуковые пакеты»).

.PARAMETER Voice
    Часть имени голоса Windows. По умолчанию мужской русский «Pavel».

.PARAMETER Name
    Имя папки пакета в sounds/.
#>
[CmdletBinding()]
param(
    [string]$Voice = 'Pavel',
    [string]$Name = 'default'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'tts.psm1') -Force

$root = Split-Path -Parent $PSScriptRoot
$dir = Join-Path (Join-Path $root 'sounds') $Name
$v = Get-TtsVoice $Voice
if (-not $v) { throw "Голос '$Voice' не найден. Установите русский голос в «Параметры → Время и язык → Речь»." }

# Чуть медленнее и ниже обычного — спокойный дворецкий.
$replies = [ordered]@{
    wake           = @('Да, сэр.', 'Слушаю, сэр.', 'К вашим услугам, сэр.')
    done           = @('Запрос выполнен, сэр.', 'Готово, сэр.')
    not_understood = @('Не понял, сэр.', 'Простите, сэр, не расслышал.')
    failed         = @('Не удалось, сэр.')
}

New-Item -ItemType Directory -Force -Path $dir | Out-Null
$toml = @("name = ""Стандартный ($($v.DisplayName))""", '', '[replies]')
foreach ($key in $replies.Keys) {
    $files = @()
    $i = 1
    foreach ($text in $replies[$key]) {
        $file = "${key}_$i.wav"
        Save-Speech -Voice $v -Text $text -Path (Join-Path $dir $file) -Rate 0.92 -Pitch -8
        Write-Host "  $file  «$text»"
        $files += """$file"""
        $i++
    }
    $toml += "$key = [$($files -join ', ')]"
}
[System.IO.File]::WriteAllText((Join-Path $dir 'pack.toml'), (($toml -join "`n") + "`n"), (New-Object System.Text.UTF8Encoding($false)))
Write-Host "Готово: $dir"
