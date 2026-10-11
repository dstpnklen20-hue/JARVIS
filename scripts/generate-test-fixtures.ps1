<#
.SYNOPSIS
    Создаёт синтетические тестовые записи tests/fixtures голосами Windows.

.DESCRIPTION
    Записи лежат в git; скрипт нужен, чтобы пересоздать их. Синтетический
    голос — только для автоматических тестов конвейера. Реальное качество
    проверяется на вашем голосе (см. отчёт этапа и README).

    wake/      — фразы со словом «Джарвис» (должно сработать);
    negative/  — обычная речь без него (не должно сработать);
    enroll/    — образцы «Джарвис» одного голоса для проверки --enroll.
#>
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'tts.psm1') -Force

$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path (Join-Path $root 'tests') 'fixtures'
$pavel = Get-TtsVoice 'Pavel'
$irina = Get-TtsVoice 'Irina'
if (-not $pavel -or -not $irina) { throw 'Нужны русские голоса Windows Pavel и Irina.' }

function Make([string]$Sub, [string]$Name, $Voice, [string]$Text, [double]$Rate = 1.0, [int]$Pitch = 0) {
    $path = Join-Path (Join-Path $out $Sub) "$Name.wav"
    Save-Speech -Voice $Voice -Text $Text -Path $path -Rate $Rate -Pitch $Pitch
    Write-Host "  $Sub/$Name.wav  «$Text»"
}

Make wake pavel_jarvis           $pavel 'Джарвис.'
Make wake irina_jarvis           $irina 'Джарвис.'
Make wake pavel_jarvis_youtube   $pavel 'Джарвис, открой ютуб.'
Make wake irina_jarvis_music     $irina 'Джарвис, включи музыку.'
Make wake pavel_jarvis_fast      $pavel 'Джарвис!' -Rate 1.4
Make wake irina_jarvis_slow      $irina 'Джарвис?' -Rate 0.7
Make wake pavel_mid_sentence     $pavel 'Слушай, Джарвис, который час?'

Make negative pavel_weather      $pavel 'Сегодня хорошая погода, давай пойдём гулять в парк, а вечером посмотрим кино.'
Make negative irina_similar      $irina 'Джордж купил новый жарочный шкаф, а Дарвин написал книгу о видах.'
Make negative pavel_numbers      $pavel 'Сделай громкость на сорок процентов и поставь таймер на пять минут.'
Make negative irina_news         $irina ('В Москве ожидается облачная погода без существенных осадков. ' +
    'Температура воздуха днём составит от пяти до семи градусов тепла. ' +
    'Ветер северо-западный, умеренный. Атмосферное давление будет медленно расти. ' +
    'В выходные синоптики обещают солнечную погоду и небольшое потепление.')
Make negative pavel_story        $pavel ('Жили-были старик со старухой у самого синего моря. ' +
    'Старик ловил неводом рыбу, а старуха пряла свою пряжу. ' +
    'Раз он в море закинул невод, пришёл невод с одною тиной. ' +
    'Он в другой раз закинул невод, пришёл невод с травой морскою.')

# Команды без «Джарвис» (режим ожидания команды и FOLLOW_UP).
# Имя файла = ожидаемый id команды из commands/ (или unknown_*).
Make commands open_youtube__pavel        $pavel 'Открой ютуб.'
Make commands open_youtube__irina_filler $irina 'Ну открой, пожалуйста, ютуб.'
Make commands open_youtube__pavel_order  $pavel 'Ютуб открой.'
Make commands open_google__irina         $irina 'Открой гугл.'
Make commands media_play_pause__pavel    $pavel 'Включи музыку.'
Make commands media_play_pause__irina    $irina 'Поставь на паузу.'
Make commands media_next__pavel          $pavel 'Следующий трек.'
Make commands media_previous__irina      $irina 'Предыдущий трек.'
Make commands media_volume_up__pavel     $pavel 'Сделай громче.'
Make commands media_volume_down__irina   $irina 'Тише.'
Make commands media_mute__pavel          $pavel 'Выключи звук.'
Make commands builtin_stop__pavel        $pavel 'Стоп.'
Make commands builtin_forget_permissions__irina $irina 'Забудь разрешения.'
Make commands unknown_bread__irina       $irina 'Купи хлеба и молока.'

# Ответы на вопрос-разрешение. Имя файла = ожидаемый ответ.
Make answers yes__pavel                  $pavel 'Да.'
Make answers yes__irina                  $irina 'Разрешаю.'
Make answers always__pavel               $pavel 'Разрешаю всегда.'
Make answers no__irina                   $irina 'Нет.'
Make answers no__pavel_cancel            $pavel 'Отмена.'
Make answers never__irina                $irina 'Никогда.'
Make commands unknown_weather__pavel     $pavel 'Какая завтра погода в Москве?'

# Образцы для --enroll: один голос, разная подача.
$i = 1
foreach ($p in @(@(1.0, 0), @(0.9, -5), @(1.15, 5), @(1.0, -10), @(0.85, 3))) {
    Make enroll "pavel_$i" $pavel 'Джарвис.' -Rate $p[0] -Pitch $p[1]
    $i++
}
Write-Host "Готово: $out"
