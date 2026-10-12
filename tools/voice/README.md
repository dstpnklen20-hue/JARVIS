# Обработка голоса

Скрипты превращают реплики голоса (например, Джарвиса из фильма) в чистые WAV и звуковой пакет.
Чужой голос — только для личного использования: записи (`data/`), результаты (`tools/voice/out/`)
и пакет (`sounds/jarvis/`) в git не попадают.

## Установка (один раз)

```powershell
winget install --id Python.Python.3.12 -e --scope user
winget install --id Gyan.FFmpeg -e
py -3.12 -m venv tools\voice\.venv
tools\voice\.venv\Scripts\python.exe -m pip install torch torchaudio torchvision --index-url https://download.pytorch.org/whl/cu128
tools\voice\.venv\Scripts\python.exe -m pip install "audio-separator[gpu]" soundfile audioread sherpa-onnx num2words
# audio-separator может заменить torch на CPU-версию — тогда повторите первую строку pip с --force-reinstall --no-deps
```

Модели разделения (~1 ГБ) скачиваются при первом запуске в `tools/voice/models/`.

## Порядок

1. Реплики в `data/jarvis-voice/` (WAV, имя файла = текст реплики).
2. `python tools\voice\clean.py` — стемы: отделение голоса, шумоподавление (видеокарта).
   ffmpeg должен быть в PATH.
3. `python tools\voice\select_best.py` — для каждой реплики выбирает самый чистый вариант, который
   распознаётся не хуже остальных; итог — `tools/voice/out/clean/`, отчёт — `out/report.csv`.
4. `python tools\voice\make_pack.py` — звуковой пакет `sounds/jarvis/`; включить:
   `[voice] pack = "jarvis"` в `config/user.toml`.

`diagnose.py "часть имени"` показывает, на каком шаге портится конкретная реплика.
