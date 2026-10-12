"""Доводка синтезированных реплик по замечаниям на слух (раунд 1):
«слишком громко и звонко», «в начале очень громко», «обрезано в конце».

- мягкий спад верхов (high-shelf −3 дБ от 6.5 кГц) — меньше «звона»;
- плавное начало 10 мс и лёгкая компрессия — резкий первый слог не бьёт по ушам;
- громкость на LOUDNESS_OFFSET ниже эталона, пик не выше −3 dBFS;
- тишина в конце не короче TAIL_SECS — фраза не звучит оборванной.

Плюс оценка вопросительной интонации: подъём тона к концу фразы (полутоны).
"""

from __future__ import annotations

from pathlib import Path

import librosa
import numpy as np
import pyloudnorm
import soundfile as sf
from scipy.signal import lfilter

LOUDNESS_OFFSET = -3.0
SHELF_HZ, SHELF_DB = 6500.0, -3.0
TAIL_SECS = 0.35
PEAK = 10 ** (-3 / 20)


def high_shelf(x: np.ndarray, rate: int, hz: float, gain_db: float) -> np.ndarray:
    """Биквад high-shelf (RBJ Audio EQ Cookbook, S = 1)."""
    a = 10 ** (gain_db / 40)
    w0 = 2 * np.pi * hz / rate
    cos, sin = np.cos(w0), np.sin(w0)
    alpha = sin / 2 * np.sqrt(2)
    sq = 2 * np.sqrt(a) * alpha
    b = [a * ((a + 1) + (a - 1) * cos + sq), -2 * a * ((a - 1) + (a + 1) * cos), a * ((a + 1) + (a - 1) * cos - sq)]
    den = [(a + 1) - (a - 1) * cos + sq, 2 * ((a - 1) - (a + 1) * cos), (a + 1) - (a - 1) * cos - sq]
    return lfilter(np.array(b) / den[0], np.array(den) / den[0], x)


def compress(x: np.ndarray, rate: int, threshold_db: float = -18.0, ratio: float = 2.0) -> np.ndarray:
    """Лёгкая компрессия по огибающей (атака 5 мс, отпускание 80 мс)."""
    env = np.abs(x)
    att, rel = np.exp(-1 / (0.005 * rate)), np.exp(-1 / (0.08 * rate))
    smooth = np.empty_like(env)
    level = 0.0
    for i, v in enumerate(env):
        level = att * level + (1 - att) * v if v > level else rel * level + (1 - rel) * v
        smooth[i] = level
    level_db = 20 * np.log10(smooth + 1e-9)
    over = np.maximum(level_db - threshold_db, 0.0)
    return x * 10 ** (-over * (1 - 1 / ratio) / 20)


def polish(path: Path, reference_lufs: float) -> None:
    x, rate = sf.read(path, dtype="float64")
    fade = int(0.01 * rate)
    x[:fade] *= np.linspace(0.0, 1.0, fade)
    x = high_shelf(x, rate, SHELF_HZ, SHELF_DB)
    x = compress(x / (np.abs(x).max() or 1.0), rate)
    meter = pyloudnorm.Meter(rate, block_size=0.2)
    x *= 10 ** ((reference_lufs + LOUDNESS_OFFSET - meter.integrated_loudness(x)) / 20)
    peak = np.abs(x).max()
    if peak > PEAK:
        x *= PEAK / peak
    # Тишина в конце — не меньше TAIL_SECS после последнего звука.
    loud = np.flatnonzero(np.abs(x) > 10 ** (-50 / 20))
    end = (loud[-1] + 1) if loud.size else len(x)
    need = int(TAIL_SECS * rate) - (len(x) - end)
    if need > 0:
        x = np.concatenate([x, np.zeros(need)])
    sf.write(path, x.astype(np.float32), rate, subtype="PCM_16")


def pitch_rise(samples: np.ndarray, rate: int = 16_000) -> float:
    """На сколько полутонов тон в последней четверти фразы выше середины."""
    f0, voiced, _ = librosa.pyin(samples, fmin=60, fmax=320, sr=rate, frame_length=1024)
    f0 = f0[voiced & np.isfinite(f0)]
    if len(f0) < 8:
        return 0.0
    n = len(f0)
    middle = np.median(f0[n // 4 : 3 * n // 4])
    tail = np.median(f0[3 * n // 4 :])
    return float(12 * np.log2(tail / middle))
