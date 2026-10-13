<#
.SYNOPSIS
    Режимы работы ноутбука Acer Predator (как в PredatorSense) для голосовых команд Джарвиса.

.DESCRIPTION
    PredatorSense переключает режимы через WMI-интерфейс Acer (root\WMI, класс
    AcerGamingFunction, настройка 0x0B): тихий = 0, обычный = 1, производительный = 4,
    турбо = 5, эко = 6 (значения — из самого PredatorSense и драйвера Linux acer-wmi).
    Этот интерфейс доступен только администратору, а Джарвис работает без прав
    администратора. Поэтому скрипт один раз создаёт задачи Планировщика с наивысшими
    правами — по одной на режим; Джарвис только запускает их (без запроса UAC).
    Сами задачи может изменить только администратор, так что подменить действие нельзя.

    Запускать в PowerShell от имени администратора:
      .\scripts\predator-modes.ps1 -Read        # какой режим сейчас (сверьте с PredatorSense)
      .\scripts\predator-modes.ps1 -Install     # создать задачи
      .\scripts\predator-modes.ps1 -Uninstall   # удалить задачи
#>
[CmdletBinding(DefaultParameterSetName = 'Read')]
param(
    [Parameter(ParameterSetName = 'Read')][switch]$Read,
    [Parameter(ParameterSetName = 'Install')][switch]$Install,
    [Parameter(ParameterSetName = 'Uninstall')][switch]$Uninstall
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$TaskFolder = '\Jarvis\'
# Имя задачи → значение режима. «актив» = турбо (ваш выбор 2026-10-13).
$Modes = [ordered]@{
    'quiet'   = 0
    'default' = 1
    'turbo'   = 5
}
$Names = @{ 0 = 'тихий'; 1 = 'обычный'; 4 = 'производительный'; 5 = 'турбо'; 6 = 'эко' }
$PlatformProfile = 0x0B

function Assert-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $admin = (New-Object Security.Principal.WindowsPrincipal $id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if (-not $admin) { throw 'Нужны права администратора: запустите PowerShell «от имени администратора».' }
}

function Get-Mode {
    $acer = Get-CimInstance -Namespace root/WMI -ClassName AcerGamingFunction | Select-Object -First 1
    if (-not $acer) { throw 'Интерфейс AcerGamingFunction не найден — это не ноутбук Acer Predator?' }
    $r = Invoke-CimMethod -InputObject $acer -MethodName GetGamingMiscSetting -Arguments @{ gmInput = [uint32]$PlatformProfile }
    $raw = [uint64]$r.gmOutput
    $status = $raw -band 0xFF
    $value = ($raw -shr 8) -band 0xFF
    if ($status -ne 0) { throw ("Чтение режима вернуло ошибку {0} (ответ 0x{1:X})" -f $status, $raw) }
    return [int]$value
}

# Команда задачи: одна строка без внешних файлов — подменять нечего.
function Get-SetCommand([int]$value) {
    # Младший байт — номер настройки, следующий — значение (как в acer-wmi).
    $packed = $PlatformProfile -bor ($value -shl 8)
    return ('$a = Get-CimInstance -Namespace root/WMI -ClassName AcerGamingFunction | Select-Object -First 1; ' +
        "`$r = Invoke-CimMethod -InputObject `$a -MethodName SetGamingMiscSetting -Arguments @{ gmInput = [uint64]$packed }; " +
        'exit [int]([uint64]$r.gmOutput -band 0xFF)')
}

Assert-Admin

if ($Install) {
    $current = Get-Mode
    $known = if ($Names.ContainsKey($current)) { $Names[$current] } else { 'неизвестный' }
    Write-Host "Сейчас режим: $current ($known)"
    if (-not $Names.ContainsKey($current)) {
        throw "Значение $current не похоже на режим PredatorSense — задачи не создаю. Пришлите это значение Claude."
    }
    $user = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit (New-TimeSpan -Minutes 1) -MultipleInstances IgnoreNew
    foreach ($name in $Modes.Keys) {
        $command = Get-SetCommand $Modes[$name]
        $action = New-ScheduledTaskAction -Execute 'powershell.exe' `
            -Argument "-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -Command `"$command`""
        Register-ScheduledTask -TaskPath $TaskFolder -TaskName "PredatorSense-$name" -Action $action `
            -Principal $principal -Settings $settings -Description "Джарвис: режим PredatorSense «$($Names[$Modes[$name]])»" -Force | Out-Null
        Write-Host "  задача $TaskFolder`PredatorSense-$name — режим $($Names[$Modes[$name]])"
    }
    Write-Host 'Готово. Проверка: «Джарвис, включи тихий режим», затем посмотрите режим в PredatorSense.'
}
elseif ($Uninstall) {
    foreach ($name in $Modes.Keys) {
        Unregister-ScheduledTask -TaskPath $TaskFolder -TaskName "PredatorSense-$name" -Confirm:$false -ErrorAction SilentlyContinue
    }
    Write-Host 'Задачи удалены.'
}
else {
    $current = Get-Mode
    $known = if ($Names.ContainsKey($current)) { $Names[$current] } else { 'неизвестный' }
    Write-Host "Сейчас режим: $current ($known). Сверьте с PredatorSense; переключите режим там и запустите снова."
}
