"""Сокращение паузы перед «сэр» (пауза от «, —» в тексте синтеза).

Находит самую длинную паузу внутри фразы (не в начале и не в конце) и
вырезает её середину так, чтобы осталась доля `factor`; края склеиваются
плавно (10 мс), без щелчка. В оценках отмечается «!»:
`not_heard_2:f5v2/ref_dry_2 = эталон !` — make_pack.py вызовет tighten().

Запуск отдельно: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\tighten.py файл.wav [доля]
"""

from __future__ import annotations

import sys
from pathlib import Path

import numpy as np
import soundfile as sf

FRAME_SECS = 0.01
# Тише этого (относительно пика) — пауза.
QUIET_DB = -35.0
MIN_GAP_SECS = 0.05
FADE_SECS = 0.01


def inner_gaps(x: np.ndarray, rate: int) -> list[tuple[int, int]]:
    """Паузы внутри фразы: [(начало, конец)] в отсчётах."""
    hop = int(rate * FRAME_SECS)
    frames = len(x) // hop
    rms = np.sqrt(np.mean(x[: frames * hop].reshape(frames, hop) ** 2, axis=1) + 1e-12)
    quiet = 20 * np.log10(rms / rms.max()) < QUIET_DB
    gaps, start = [], None
    for i, q in enumerate(quiet):
        if q and start is None:
            start = i
        elif not q and start is not None:
            # Пауза в самом начале — не внутри фразы.
            if start > 0 and (i - start) * FRAME_SECS >= MIN_GAP_SECS:
                gaps.append((start * hop, i * hop))
            start = None
    # Незакрытая пауза — хвост в конце, тоже не внутри.
    return gaps


def tighten(path: Path, factor: float = 0.5, out: Path | None = None) -> tuple[float, float]:
    """Сокращает самую длинную внутреннюю паузу до `factor` от прежней.
    Возвращает (было, стало) в секундах; паузы нет — (0, 0), файл не меняется."""
    x, rate = sf.read(str(path), dtype="float32")
    mono = x if x.ndim == 1 else x.mean(axis=1)
    gaps = inner_gaps(mono, rate)
    if not gaps:
        return 0.0, 0.0
    start, end = max(gaps, key=lambda g: g[1] - g[0])
    length = end - start
    fade = int(rate * FADE_SECS)
    # Склейка перекрывает `fade` отсчётов — они тоже уходят из паузы.
    cut = int(length * (1 - factor)) - fade
    if cut <= 0 or (length - cut) // 2 < fade:
        return length / rate, length / rate
    # Вырезаем середину паузы: [a, b) уходит, края склеиваются перекрёстным затуханием.
    a = start + (length - cut) // 2
    b = a + cut
    ramp = np.linspace(1.0, 0.0, fade, dtype=np.float32)
    if x.ndim > 1:
        ramp = ramp[:, None]
    joined = x[a - fade : a] * ramp + x[b : b + fade] * (1 - ramp)
    y = np.concatenate([x[: a - fade], joined, x[b + fade :]])
    sf.write(str(out or path), y, rate, subtype="PCM_16")
    return length / rate, (length - cut - fade) / rate


def main() -> None:
    path = Path(sys.argv[1])
    factor = float(sys.argv[2]) if len(sys.argv) > 2 else 0.5
    before, after = tighten(path, factor)
    print(f"{path.name}: пауза {before:.2f} с → {after:.2f} с")


if __name__ == "__main__":
    main()
