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

Восстановление речи (ClearerVoice) — в отдельном окружении: ему нужны другие версии numpy.

```powershell
py -3.12 -m venv tools\voice\.venv-enhance
tools\voice\.venv-enhance\Scripts\python.exe -m pip install clearvoice
tools\voice\.venv-enhance\Scripts\python.exe -m pip install --force-reinstall --no-deps torch torchaudio torchvision --index-url https://download.pytorch.org/whl/cu128
```

Оценка качества — DNSMOS: `tools/voice/models/dnsmos/sig_bak_ovr.onnx` из
github.com/microsoft/DNS-Challenge (папка `DNSMOS/DNSMOS`).

## Порядок

1. Реплики в `data/jarvis-voice/` (WAV, имя файла = текст реплики).
2. `.venv\...\python tools\voice\clean.py` — отделение голоса BS-RoFormer (видеокарта; ffmpeg в PATH).
3. `.venv-enhance\...\python tools\voice\enhance.py` — восстановление речи MossFormer2 по исходнику
   и по стему голоса.
4. `.venv\...\python tools\voice\select_best.py` — для каждой реплики выбирает вариант с лучшей
   оценкой DNSMOS, где слова не потерялись, а тембр близок к эталону («Запрос выполнен сэр»);
   выравнивает громкость под эталон. Итог — `out/clean/`, по уровням качества —
   `out/по уровням/`, отчёт — `out/report.csv`.
5. `.venv\...\python tools\voice\make_pack.py` — звуковой пакет `sounds/jarvis/`; включить:
   `[voice] pack = "jarvis"` в `config/user.toml`.

`score.py` — все варианты всех реплик с оценками; `diagnose.py "часть имени"` — на каком шаге
портится конкретная реплика.

## Результат на архиве «Jarvis Sound Pack» (60 реплик)

DNSMOS OVRL (1–5): исходники в среднем 2.0, после выбора ~2.75, эталон 3.4.
Уровня эталона (≥ 3.0) — 23 реплики (1.2 мин), средних — 19, слабых — 18: они испорчены в самом
фильме, восстановление поднимает их лишь до ~2.0–2.4.
