# Обучение голоса Piper на записях Джарвиса (в Docker, на видеокарте).
# Данные — tools\voice\out\piper (подключается в контейнер как /work):
#   dataset\metadata.csv + dataset\wavs\  — из synth.py и prepare.py
#   ckpt\dmitri.ckpt                      — заготовка (веса dmitri medium)
# Результат — out\piper\train\lightning_logs\version_N\checkpoints\*.ckpt
#
# Первый раунд (с весов dmitri):   .\train.ps1 -Epochs 60
# Продолжить с последнего чекпоинта: .\train.ps1 -Epochs 150 -Resume <путь в /work/...>
# Выгрузить в .onnx:                 .\train.ps1 -Export <путь к .ckpt в /work/...> -Name jarvis-60
param(
    [int]$Epochs = 60,
    [int]$BatchSize = 16,
    [string]$Dataset = "dataset",
    [string]$Resume = "",
    [string]$Export = "",
    [string]$Name = "jarvis",
    [switch]$DryRun
)
# Не "Stop": PowerShell 5.1 считает ошибкой любой вывод docker в stderr (предупреждения).
$work = (Resolve-Path "$PSScriptRoot\..\out\piper").Path
Copy-Item "$PSScriptRoot\train_config.yaml" "$work\train_config.yaml" -Force
Copy-Item "$PSScriptRoot\run_train.py" "$work\run_train.py" -Force
$docker = @("run", "--rm", "--gpus", "all", "--shm-size", "4g", "-v", "${work}:/work", "jarvis-piper-train")

if ($Export) {
    New-Item -ItemType Directory -Force "$work\export" | Out-Null
    & docker @docker python -m piper.train.export_onnx --checkpoint $Export --output-file "/work/export/$Name.onnx"
    exit $LASTEXITCODE
}

$cmd = @(
    "python", "/work/run_train.py", "fit",
    "--config", "/work/train_config.yaml",
    "--data.voice_name", "jarvis",
    "--data.csv_path", "/work/$Dataset/metadata.csv",
    "--data.audio_dir", "/work/$Dataset/wavs",
    "--model.sample_rate", "22050",
    "--data.espeak_voice", "ru",
    "--data.cache_dir", "/work/cache/$Dataset",
    "--data.config_path", "/work/train/config.json",
    "--data.batch_size", "$BatchSize",
    "--data.num_workers", "4",
    "--model.mos_metric", "none",
    "--trainer.accelerator", "gpu",
    "--trainer.devices", "1",
    "--trainer.max_epochs", "$Epochs",
    "--trainer.default_root_dir", "/work/train"
)
if ($Resume) { $cmd += @("--ckpt_path", $Resume) } else { $cmd += @("--model.warmstart_ckpt", "/work/ckpt/dmitri.ckpt") }
if ($DryRun) { $cmd += @("--trainer.fast_dev_run", "true") }
& docker @docker @cmd
exit $LASTEXITCODE
