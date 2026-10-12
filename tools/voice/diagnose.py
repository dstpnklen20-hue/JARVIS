"""На каком шаге очистки портится реплика: распознаёт исходник, стем голоса
(BS-RoFormer), стем после шумоподавления и итог — для указанных файлов.

Запуск: python tools\\voice\\diagnose.py "часть имени" ["часть имени" ...]
"""

from __future__ import annotations

import sys
from pathlib import Path

import numpy as np

from verify import OUT, RATE, SRC, load, recognizer, transcribe


def rms_db(x: np.ndarray) -> float:
    return float(20 * np.log10(np.sqrt((x**2).mean() + 1e-12)))


def main() -> None:
    rec = recognizer()
    for part in sys.argv[1:]:
        for orig in sorted(SRC.rglob(f"*{part}*.wav")):
            name = "__".join(orig.relative_to(SRC).with_suffix("").parts)
            print(f"\n{orig.stem}")
            for label, path in [
                ("исходник", orig),
                ("голос", OUT / "stems" / "vocals" / f"{name}.wav"),
                ("без шума", OUT / "stems" / "dry" / f"{name}.wav"),
                ("итог", OUT / "clean" / f"{name}.wav"),
            ]:
                if not path.exists():
                    print(f"  {label:<9} нет файла")
                    continue
                x = load(path)
                print(
                    f"  {label:<9} {len(x) / RATE:4.1f} с  {rms_db(x):6.1f} дБ  «{transcribe(rec, x / (np.abs(x).max() or 1))}»"
                )


if __name__ == "__main__":
    main()
