#!/usr/bin/env bash
# Цепочка после первого раунда: выгрузка e40/e60 → страница и оценки →
# вторые дубли в обычном темпе → очищенный набор → второй раунд обучения.
# Запуск (Git Bash): bash tools/voice/piper/round2.sh <файл-вывода-раунда-1>
set -u
cd "$(dirname "$0")/../../.."
P=tools/voice/out/piper
PY=tools/voice/.venv/Scripts/python.exe
PYF5=tools/voice/.venv-f5/Scripts/python.exe
export PYTHONIOENCODING=utf-8
# Пути /work/... — для контейнера; Git Bash не должен превращать их в C:/Program Files/Git/work.
export MSYS_NO_PATHCONV=1

until grep -q 'обучение завершено' "$1" 2>/dev/null; do sleep 30; done
echo "== раунд 1 завершён"

ckdir=$(ls -d $P/train/lightning_logs/version_* | sort -V | tail -1)/checkpoints
for e in 39 59; do
  if [ -f "$ckdir/epoch=$e.ckpt" ]; then
    cp "$ckdir/epoch=$e.ckpt" "$P/ckpt/jarvis-e$((e + 1)).ckpt"
    powershell -NoProfile -ExecutionPolicy Bypass -File tools/voice/piper/train.ps1 \
      -Export "/work/ckpt/jarvis-e$((e + 1)).ckpt" -Name "jarvis-e$((e + 1))" > /dev/null 2>&1
    echo "== выгружен jarvis-e$((e + 1))"
  fi
done
$PY tools/voice/piper/listen_piper.py < /dev/null 2>&1 | grep -E 'готов|Страница'
$PY tools/voice/piper/score_piper.py < /dev/null 2>&1 | grep -v -i warn

echo "== вторые дубли (обычный темп)"
$PYF5 tools/voice/piper/synth.py --ref dry --speed 1.0 --out raw_b --seed 10000 < /dev/null 2>&1 \
  | grep --line-buffered -E '^(1|2|3|4|5|6|7)[05]0/|^773/|^фраз|Traceback|Error'
echo "== очищенный набор"
$PY tools/voice/piper/prepare.py --raw raw,raw_b --min-score 0.9 --drop-worst 0.15 --out dataset_v2 < /dev/null 2>&1 \
  | grep -v -i warn | tail -2

echo "== раунд 2"
powershell -NoProfile -ExecutionPolicy Bypass -File tools/voice/piper/train.ps1 -Dataset dataset_v2 -Epochs 100 \
  > $P/train_round2.log 2>&1
echo "== раунд 2 завершён, код $?"
