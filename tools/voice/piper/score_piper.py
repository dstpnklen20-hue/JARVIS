"""Объективная сводка по версиям голоса со страницы piper_listen.

DNSMOS: SIG — чистота самого голоса (хрип, металл, искажения), BAK — фон,
OVRL — общее. «Тембр» — сходство голоса (CAM++) со средним по обучающим
записям. Для сравнения — те же оценки у 20 обучающих записей (к ним голос
стремится) и у dmitri.

Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\piper\\score_piper.py
"""

from __future__ import annotations

import json
import random
import sys
from pathlib import Path

import numpy as np
import soundfile as sf
import torch
import torchaudio.functional as AF

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from quality import Quality  # noqa: E402
from verify import OUT, RATE  # noqa: E402

LISTEN = OUT / "piper_listen"
DATASET = OUT / "piper" / "dataset" / "wavs"


def load16(path: Path) -> np.ndarray:
    audio, rate = sf.read(path, dtype="float32")
    return AF.resample(torch.from_numpy(audio), rate, RATE).numpy()


def main() -> None:
    q = Quality()
    train = random.Random(3).sample(sorted(DATASET.glob("*.wav")), 20)
    target = np.mean([q.embed(load16(p)) for p in train], axis=0)
    target /= np.linalg.norm(target)

    groups = {"обучающие записи": train}
    for name in json.loads((LISTEN / "voices.json").read_text(encoding="utf-8")):
        groups[name] = sorted(LISTEN.glob(f"*__{name}.wav"))

    print(f"{'':<18} {'SIG':>5} {'BAK':>5} {'OVRL':>5} {'тембр':>6}")
    for name, files in groups.items():
        mos = np.array([q.mos(load16(p)) for p in files])
        timbre = np.mean([float(q.embed(load16(p)) @ target) for p in files])
        sig, bak, ovr = mos.mean(axis=0)
        print(f"{name:<18} {sig:5.2f} {bak:5.2f} {ovr:5.2f} {timbre:6.2f}")


if __name__ == "__main__":
    main()
