"""Набор для дообучения на настоящих репликах Джарвиса из фильма.

60 очищенных реплик (out/clean, ~2.3 мин) — слишком мало, чтобы учиться только
на них, поэтому смесь: настоящие реплики повторены несколько раз (тембр) +
часть очищенного набора ESpeech (dataset_v2 — произношение и охват звуков).
Текст — из имён файлов с поправками (числа словами, латиница по-русски,
опечатки); у двух реплик без текста — распознанный и проверенный.

Результат — out/piper/dataset_mix (wavs + metadata.csv).
Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\piper\\prepare_real.py [--repeat 4] [--synthetic 300]
"""

from __future__ import annotations

import argparse
import random
import re
import shutil
import sys
from pathlib import Path

import numpy as np
import pyloudnorm
import soundfile as sf
import torch
import torchaudio.functional as AF

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from prepare import TARGET_LUFS, TARGET_RATE, trim  # noqa: E402
from verify import OUT  # noqa: E402

PIPER = OUT / "piper"
FIXES = {
    "Джарвис - приветствие": "С возвращением, сэр.",
    "11 %": "Одиннадцать процентов.",
    "2 %": "Два процента.",
    "7 %": "Семь процентов.",
    "Сэр, энергия 19 %": "Сэр, энергия девятнадцать процентов.",
    "Энергия 13%": "Энергия тринадцать процентов.",
    "Энергия 15 %": "Энергия пятнадцать процентов.",
    "Энергия 48% и падает сэр": "Энергия сорок восемь процентов и падает, сэр.",
    "Рассказ про Ивана Ванко": (
        "Антон Ванко был советским физиком, бежавшим в США в тысяча девятьсот шестьдесят третьем году. "
        "Однако уже в шестьдесят седьмом он был обвинён в шпионаже и депортирован.|"
        "Его сын Иван,|также физик, был признан виновным в продаже произведённого в Советском Союзе "
        "оружейного плутония Пакистану и отбыл пятнадцать лет в лагере на Урале."
    ),
    "Сохранить его в центральной базе данных Stark Industries": "Сохранить его в центральной базе данных Старк Индастриз.",
    "Для полетов на другие планеты слелует усовершенствовать экзосистемы": (
        "Для полётов на другие планеты следует усовершенствовать экзосистемы."
    ),
    "Приближается мисс поттс =)": "Приближается мисс Поттс.",
    "Мы работаем над проектом сэр 2": "Мы работаем над проектом, сэр.",
}


def text_for(stem: str) -> str:
    name = stem.split("__")[-1].strip()
    if name in FIXES:
        return FIXES[name]
    name = re.sub(r"\s*\((второй|второе)\)\s*$", "", name).strip()
    name = re.sub(r"\s+сэр$", ", сэр", name)
    if not name.endswith((".", "!", "?")):
        name += "?" if name.startswith(("Чего", "О чем")) else "."
    return name[0].upper() + name[1:]


def split_sentences(audio: np.ndarray, rate: int, text: str) -> list[tuple[np.ndarray, str]]:
    """Длинную реплику режет по самым длинным паузам. Места разреза отмечены
    в тексте «|» (по одному на паузу — так, как реплика произнесена)."""
    sentences = [s.strip() for s in text.split("|") if s.strip()]
    if len(sentences) < 2:
        return [(audio, text)]
    frame = int(0.01 * rate)
    n = len(audio) // frame
    rms = np.sqrt((audio[: n * frame].reshape(n, frame) ** 2).mean(axis=1)) + 1e-9
    quiet = 20 * np.log10(rms / rms.max()) < -35
    pauses = []  # (длина, середина) участков тишины
    start = None
    for i, q in enumerate(quiet):
        if q and start is None:
            start = i
        elif not q and start is not None:
            pauses.append((i - start, (start + i) // 2))
            start = None
    cuts = sorted(mid for _, mid in sorted(pauses, reverse=True)[: len(sentences) - 1])
    if len(cuts) != len(sentences) - 1:
        return [(audio, text)]
    bounds = [0, *(c * frame for c in cuts), len(audio)]
    return [(audio[a:b], s) for a, b, s in zip(bounds[:-1], bounds[1:], sentences, strict=True)]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repeat", type=int, default=4, help="сколько раз повторить настоящие реплики")
    parser.add_argument("--synthetic", type=int, default=300, help="сколько фраз взять из dataset_v2")
    args = parser.parse_args()

    dest = PIPER / "dataset_mix"
    if dest.exists():
        shutil.rmtree(dest)
    wavs = dest / "wavs"
    wavs.mkdir(parents=True)
    meter = pyloudnorm.Meter(TARGET_RATE)
    lines = []
    real_secs = 0.0
    for k, src in enumerate(sorted((OUT / "clean").glob("*.wav"))):
        audio, rate = sf.read(src, dtype="float32")
        if audio.ndim > 1:
            audio = audio.mean(axis=1)
        for part, (piece, text) in enumerate(split_sentences(trim(audio, rate), rate, text_for(src.stem))):
            out = AF.resample(torch.from_numpy(trim(piece, rate)), rate, TARGET_RATE).numpy()
            out = out * 10 ** ((TARGET_LUFS - meter.integrated_loudness(out)) / 20)
            peak = float(np.abs(out).max())
            if peak > 0.89:
                out = out * (0.89 / peak)
            real_secs += len(out) / TARGET_RATE
            for r in range(args.repeat):
                name = f"real{k:02d}{part}_{r}.wav"
                sf.write(wavs / name, out, TARGET_RATE, subtype="PCM_16")
                lines.append(f"{name}|{text}")

    synthetic = (PIPER / "dataset_v2" / "metadata.csv").read_text(encoding="utf-8").splitlines()
    picked = random.Random(5).sample(synthetic, min(args.synthetic, len(synthetic)))
    for line in picked:
        name, text = line.split("|", 1)
        shutil.copy(PIPER / "dataset_v2" / "wavs" / name, wavs / f"syn{name}")
        lines.append(f"syn{name}|{text}")

    (dest / "metadata.csv").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"настоящих реплик {real_secs / 60:.1f} мин × {args.repeat}, фраз ESpeech {len(picked)}; всего строк {len(lines)}")


if __name__ == "__main__":
    main()
