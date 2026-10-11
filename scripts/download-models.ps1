<#
.SYNOPSIS
    Скачивает модели и нативные библиотеки Джарвиса с проверкой SHA-256.

.DESCRIPTION
    Список ресурсов, URL и контрольные суммы — в scripts/assets.json
    (его же читает `jarvis --check`). Для каждого ресурса скрипт:
      1. скачивает архив с докачкой при обрыве (curl.exe);
      2. проверяет размер и SHA-256 — при несовпадении удаляет файл и падает;
      3. распаковывает (tar.exe) и кладёт метку .jarvis-verified с хешем.
    Уже установленные ресурсы с верной меткой пропускаются.

.PARAMETER Full
    Также скачать необязательные ресурсы (полная модель Vosk, ~1.9 ГБ).

.PARAMETER Force
    Переустановить ресурсы, даже если они уже на месте.

.PARAMETER ModelsDir
    Папка моделей. Должна совпадать с paths.models в конфиге.

.PARAMETER LibDir
    Папка нативных библиотек. Должна совпадать с paths.native_libs.

.PARAMETER ArchiveDir
    Где хранить скачанные архивы. Если нужный архив там уже есть и его хеш
    верный, повторно он не скачивается.

.PARAMETER KeepArchives
    Не удалять архивы после распаковки.

.EXAMPLE
    .\scripts\download-models.ps1
    .\scripts\download-models.ps1 -Full
#>
[CmdletBinding()]
param(
    [switch]$Full,
    [switch]$Force,
    [string]$ModelsDir = 'models',
    [string]$LibDir = 'lib',
    [string]$ArchiveDir = 'models\.downloads',
    [switch]$KeepArchives
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Root = Split-Path -Parent $PSScriptRoot
$MarkerName = '.jarvis-verified'

function Resolve-InRoot([string]$Path) {
    if ([System.IO.Path]::IsPathRooted($Path)) { return $Path }
    return Join-Path $Root $Path
}

function Get-Sha256([string]$Path) {
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

function Format-Size([long]$Bytes) {
    if ($Bytes -ge 1GB) { return '{0:N2} ГБ' -f ($Bytes / 1GB) }
    return '{0:N1} МБ' -f ($Bytes / 1MB)
}

foreach ($tool in 'curl.exe', 'tar.exe') {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        throw "Не найден $tool (входит в Windows 10 1803+ и Windows 11)."
    }
}

$manifestPath = Join-Path $PSScriptRoot 'assets.json'
$manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
$bases = @{ models = Resolve-InRoot $ModelsDir; native_libs = Resolve-InRoot $LibDir }
$archives = Resolve-InRoot $ArchiveDir
New-Item -ItemType Directory -Force -Path $archives | Out-Null

$failed = @()
foreach ($asset in $manifest.assets) {
    if (-not $asset.required -and -not $Full) {
        Write-Host "[ -- ] $($asset.name): пропущено (нужен флаг -Full, $(Format-Size $asset.size))"
        continue
    }

    $base = $bases[$asset.base]
    if (-not $base) { throw "assets.json: неизвестная база '$($asset.base)' у $($asset.id)" }
    $target = Join-Path $base $asset.dir
    $marker = Join-Path $target $MarkerName
    $expected = $asset.sha256.ToLowerInvariant()

    if (-not $Force -and (Test-Path -LiteralPath $marker) -and
        ((Get-Content -LiteralPath $marker -Raw).Trim().ToLowerInvariant() -eq $expected)) {
        Write-Host "[ OK ] $($asset.name): уже установлено"
        continue
    }

    try {
        $fileName = [System.IO.Path]::GetFileName(([uri]$asset.url).AbsolutePath)
        $archive = Join-Path $archives $fileName

        $haveGood = (Test-Path -LiteralPath $archive) -and
            ((Get-Item -LiteralPath $archive).Length -eq $asset.size) -and
            ((Get-Sha256 $archive) -eq $expected)

        if ($haveGood) {
            Write-Host "[....] $($asset.name): архив уже скачан, хеш верный"
        } else {
            # Файл больше ожидаемого докачать нельзя — начинаем заново.
            if ((Test-Path -LiteralPath $archive) -and (Get-Item -LiteralPath $archive).Length -ge $asset.size) {
                Remove-Item -LiteralPath $archive -Force
            }
            Write-Host "[....] $($asset.name): скачиваю $(Format-Size $asset.size)"
            & curl.exe --location --fail --retry 5 --retry-delay 3 --continue-at - --progress-bar `
                --output $archive $asset.url
            if ($LASTEXITCODE -ne 0) { throw "curl завершился с кодом $LASTEXITCODE" }

            $size = (Get-Item -LiteralPath $archive).Length
            if ($size -ne $asset.size) {
                throw "размер $size байт, ожидалось $($asset.size). Запустите скрипт ещё раз — загрузка продолжится."
            }
            $actual = Get-Sha256 $archive
            if ($actual -ne $expected) {
                Remove-Item -LiteralPath $archive -Force
                throw "SHA-256 не совпадает: получено $actual, ожидалось $expected. Файл удалён."
            }
        }

        Write-Host "[....] $($asset.name): распаковываю"
        $staging = Join-Path $base ".extract-$($asset.id)"
        if (Test-Path -LiteralPath $staging) { Remove-Item -LiteralPath $staging -Recurse -Force }
        New-Item -ItemType Directory -Force -Path $staging | Out-Null
        & tar.exe -xf $archive -C $staging
        if ($LASTEXITCODE -ne 0) { throw "tar завершился с кодом $LASTEXITCODE" }

        $extracted = Join-Path $staging $asset.dir
        if (-not (Test-Path -LiteralPath $extracted -PathType Container)) {
            throw "в архиве нет папки $($asset.dir)"
        }
        foreach ($f in $asset.files) {
            if (-not (Test-Path -LiteralPath (Join-Path $extracted $f) -PathType Leaf)) {
                throw "в архиве нет файла $($asset.dir)/$f"
            }
        }

        # Метка пишется до переноса: папка появляется на месте уже проверенной.
        [System.IO.File]::WriteAllText((Join-Path $extracted $MarkerName), "$expected`n")
        if (Test-Path -LiteralPath $target) { Remove-Item -LiteralPath $target -Recurse -Force }
        Move-Item -LiteralPath $extracted -Destination $target
        Remove-Item -LiteralPath $staging -Recurse -Force

        if (-not $KeepArchives) { Remove-Item -LiteralPath $archive -Force }
        Write-Host "[ OK ] $($asset.name): $target"
    } catch {
        Write-Host "[FAIL] $($asset.name): $($_.Exception.Message)" -ForegroundColor Red
        $failed += $asset.id
    }
}

if ((Test-Path -LiteralPath $archives) -and -not (Get-ChildItem -LiteralPath $archives -Force)) {
    Remove-Item -LiteralPath $archives -Force
}

if ($failed.Count -gt 0) {
    Write-Host "`nНе установлено: $($failed -join ', ')" -ForegroundColor Red
    exit 1
}
Write-Host "`nГотово. Проверка: cargo run -- --check"
