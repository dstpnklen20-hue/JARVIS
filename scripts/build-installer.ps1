<#
.SYNOPSIS
    Собирает установщик Джарвиса: target\installer\JarvisSetup.exe.

.DESCRIPTION
    1. cargo build --release.
    2. Складывает файлы программы в target\installer\stage: jarvis.exe, jarvisw.exe,
       config\default.toml, пакеты команд (кроме личного commands\my), стандартный
       звуковой пакет, модели для работы (распознавание, детекторы речи, образец голоса,
       голос dmitri) и scripts\assets.json + download-models.ps1 (остальные голоса).
    3. Собирает установщик Inno Setup 6 (installer\jarvis.iss).

    Личное (голос Джарвиса, commands\my, ваши настройки и данные) в установщик не
    попадает. -Personal — собрать установщик для ЭТОГО компьютера вместе с голосом
    Джарвиса и личными командами (JarvisSetup-personal.exe; никому не передавать:
    голос — только для личного использования).

.PARAMETER Personal
    Добавить sounds\jarvis и commands\my.
#>
[CmdletBinding()]
param(
    [switch]$Personal
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$stage = Join-Path $root 'target\installer\stage'
$iscc = @(
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) {
    throw 'Не найден Inno Setup 6: winget install --id JRSoftware.InnoSetup -e --scope user'
}

Write-Host '1/3 Сборка...'
Push-Location $root
try {
    # Сообщения cargo идут в stderr; через cmd они не превращаются в ошибки PowerShell 5.1.
    & cmd /c "cargo build --release 2>&1"
    if ($LASTEXITCODE -ne 0) { throw 'cargo build --release не удался' }
} finally {
    Pop-Location
}

Write-Host '2/3 Файлы программы...'
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force -Path $stage | Out-Null

function Copy-Into([string]$from, [string]$to) {
    $src = Join-Path $root $from
    if (-not (Test-Path $src)) { throw "нет $from — сначала scripts\download-models.ps1" }
    $dst = Join-Path $stage $to
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dst) | Out-Null
    Copy-Item $src $dst -Recurse -Force
}

Copy-Into 'target\release\jarvis.exe' 'jarvis.exe'
Copy-Into 'target\release\jarvisw.exe' 'jarvisw.exe'
Copy-Into 'config\default.toml' 'config\default.toml'
Copy-Into 'README.md' 'README.md'
Copy-Into 'scripts\assets.json' 'scripts\assets.json'
Copy-Into 'scripts\download-models.ps1' 'scripts\download-models.ps1'
Copy-Into 'sounds\default' 'sounds\default'
foreach ($pack in Get-ChildItem (Join-Path $root 'commands') -Directory) {
    if ($pack.Name -eq 'my' -and -not $Personal) { continue }
    Copy-Into "commands\$($pack.Name)" "commands\$($pack.Name)"
}
# Модели, нужные для работы; остальные голоса — download-models.ps1 -Full.
$assets = (Get-Content (Join-Path $root 'scripts\assets.json') -Raw -Encoding UTF8 | ConvertFrom-Json).assets
foreach ($asset in $assets) {
    $buildOnly = $asset.PSObject.Properties.Name -contains 'build_only' -and $asset.build_only
    if ($asset.base -ne 'models' -or $buildOnly) { continue }
    if (-not $asset.required -and $asset.id -ne 'vad-ten') { continue }
    Copy-Into "models\$($asset.dir)" "models\$($asset.dir)"
}
if ($Personal) {
    Copy-Into 'sounds\jarvis' 'sounds\jarvis'
}

$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$name = if ($Personal) { 'JarvisSetup-personal' } else { 'JarvisSetup' }
$size = (Get-ChildItem $stage -Recurse -File | Measure-Object Length -Sum).Sum / 1MB
Write-Host ("   {0:N0} МБ, версия {1}" -f $size, $version)

Write-Host '3/3 Установщик...'
& $iscc "/DStage=$stage" "/DAppVersion=$version" "/DOutputName=$name" "/Q" (Join-Path $root 'installer\jarvis.iss')
if ($LASTEXITCODE -ne 0) { throw 'Inno Setup не собрал установщик' }
$out = Join-Path $root "target\installer\$name.exe"
Write-Host ("Готово: {0} ({1:N0} МБ)" -f $out, ((Get-Item $out).Length / 1MB))
