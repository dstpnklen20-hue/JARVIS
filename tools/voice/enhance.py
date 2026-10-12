"""Восстановление речи ClearerVoice MossFormer2_SE_48K: убирает искажения и
остаточный шум, которые не убирает отделение голоса.

Запускается из отдельного окружения (у ClearerVoice другие версии numpy):
  tools\\voice\\.venv-enhance\\Scripts\\python.exe tools\\voice\\enhance.py
Стемы: out/stems/se_orig (по исходнику), out/stems/se_vocals (по стему голоса).
"""

from __future__ import annotations

import sys
import tempfile
from pathlib import Path

import numpy as np
import soundfile as sf
from clearvoice import ClearVoice

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / "data" / "jarvis-voice"
OUT = ROOT / "tools" / "voice" / "out"
SKIP = ("Песня целиком",)
RATE = 48_000


def main() -> None:
    cv = ClearVoice(task="speech_enhancement", model_names=["MossFormer2_SE_48K"])
    files = sorted(p for p in SRC.rglob("*.wav") if not any(s in p.stem for s in SKIP))
    jobs = []
    for f in files:
        name = "__".join(f.relative_to(SRC).with_suffix("").parts)
        jobs.append((f, OUT / "stems" / "se_orig" / f"{name}.wav"))
        jobs.append((OUT / "stems" / "vocals" / f"{name}.wav", OUT / "stems" / "se_vocals" / f"{name}.wav"))
    with tempfile.TemporaryDirectory() as tmp:
        for src, dst in jobs:
            if dst.exists() or not src.exists():
                continue
            dst.parent.mkdir(parents=True, exist_ok=True)
            # Модели проще отдать ASCII-путь и моно 48 кГц.
            audio, rate = sf.read(src, always_2d=True, dtype="float32")
            mono = audio.mean(axis=1)
            if rate != RATE:
                import librosa

                mono = librosa.resample(mono, orig_sr=rate, target_sr=RATE)
            tmp_in = Path(tmp) / "in.wav"
            sf.write(tmp_in, mono, RATE)
            out = np.asarray(cv(input_path=str(tmp_in), online_write=False)).squeeze()
            sf.write(dst, out.astype(np.float32), RATE)
            print(f"  {dst.parent.name}/{dst.stem[:60]}", flush=True)
    print("Готово")


if __name__ == "__main__":
    sys.exit(main())
