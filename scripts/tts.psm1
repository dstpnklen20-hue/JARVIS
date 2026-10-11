<#
    Синтез речи голосами Windows (WinRT Windows.Media.SpeechSynthesis) в WAV.
    Используется скриптами generate-voice-pack.ps1 и generate-test-fixtures.ps1.
    Работает в Windows PowerShell 5.1.
#>

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Runtime.WindowsRuntime
$null = [Windows.Media.SpeechSynthesis.SpeechSynthesizer, Windows.Media.SpeechSynthesis, ContentType = WindowsRuntime]
$null = [Windows.Storage.Streams.DataReader, Windows.Storage.Streams, ContentType = WindowsRuntime]

$script:AsTask = [System.WindowsRuntimeSystemExtensions].GetMethods() |
    Where-Object { $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and
        $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' } |
    Select-Object -First 1

function Wait-WinRt($Operation, [Type]$ResultType) {
    $task = $script:AsTask.MakeGenericMethod($ResultType).Invoke($null, @($Operation))
    $null = $task.Wait(-1)
    return $task.Result
}

<#
.SYNOPSIS
    Голос по части имени (например, 'Pavel' или 'Irina'); $null — нет такого.
#>
function Get-TtsVoice([string]$NamePart) {
    [Windows.Media.SpeechSynthesis.SpeechSynthesizer]::AllVoices |
        Where-Object { $_.DisplayName -like "*$NamePart*" } |
        Select-Object -First 1
}

<#
.SYNOPSIS
    Синтезирует фразу в WAV-файл (PCM 16 бит, моно).
.PARAMETER Rate
    Скорость речи: 1.0 — обычная.
.PARAMETER Pitch
    Высота тона в процентах относительно обычной (например, -10).
#>
function Save-Speech {
    param(
        [Parameter(Mandatory)] $Voice,
        [Parameter(Mandatory)] [string]$Text,
        [Parameter(Mandatory)] [string]$Path,
        [double]$Rate = 1.0,
        [int]$Pitch = 0
    )
    $synth = New-Object Windows.Media.SpeechSynthesis.SpeechSynthesizer
    try {
        $synth.Voice = $Voice
        $escaped = [System.Security.SecurityElement]::Escape($Text)
        $pitchText = if ($Pitch -ge 0) { "+$Pitch%" } else { "$Pitch%" }
        $rateText = $Rate.ToString([System.Globalization.CultureInfo]::InvariantCulture)
        $ssml = "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='ru-RU'>" +
            "<prosody rate='$rateText' pitch='$pitchText'>$escaped</prosody></speak>"
        $stream = Wait-WinRt ($synth.SynthesizeSsmlToStreamAsync($ssml)) ([Windows.Media.SpeechSynthesis.SpeechSynthesisStream])
        $reader = New-Object Windows.Storage.Streams.DataReader($stream.GetInputStreamAt(0))
        $size = [uint32]$stream.Size
        $null = Wait-WinRt ($reader.LoadAsync($size)) ([uint32])
        $bytes = New-Object byte[] $size
        $reader.ReadBytes($bytes)
        $dir = Split-Path -Parent $Path
        if ($dir) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
        [System.IO.File]::WriteAllBytes($Path, $bytes)
    } finally {
        $synth.Dispose()
    }
}

Export-ModuleMember -Function Get-TtsVoice, Save-Speech
