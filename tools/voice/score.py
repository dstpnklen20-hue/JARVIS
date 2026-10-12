"""Сравнение вариантов очистки с эталоном («Запрос выполнен сэр»).

Для каждой реплики и каждого варианта (стемы в out/stems/*, исходник):
DNSMOS SIG/BAK/OVRL, похожесть тембра на эталон, разборчивость.
Запуск: python tools\\voice\\score.py [эталон]
"""

from __future__ import annotations

import sys

import numpy as np

from quality import Quality
from select_best import SKIP, reference, similarity
from verify import OUT, SRC, load, recognizer, transcribe

REFERENCE = "Запрос выполнен сэр"


def main() -> None:
    ref_name = sys.argv[1] if len(sys.argv) > 1 else REFERENCE
    q = Quality()
    rec = recognizer()
    stages = sorted(p.name for p in (OUT / "stems").iterdir() if p.is_dir())
    files = sorted(p for p in SRC.rglob("*.wav") if not any(s in p.stem for s in SKIP))
    ref_file = next(f for f in files if f.stem.strip() == ref_name)
    ref_name_full = "__".join(ref_file.relative_to(SRC).with_suffix("").parts)
    # Эталон — его выбранный (очищенный) вариант.
    ref_emb = q.embed(load(OUT / "clean" / f"{ref_name_full}.wav"))

    print(f"Варианты: исходник, {', '.join(stages)}")
    totals: dict[str, list] = {}
    for f in files:
        name = "__".join(f.relative_to(SRC).with_suffix("").parts)
        expected = reference(f.stem)
        line = []
        for stage in ["исходник", *stages]:
            path = f if stage == "исходник" else OUT / "stems" / stage / f"{name}.wav"
            if not path.exists():
                continue
            x = load(path)
            x = x / (np.abs(x).max() or 1.0)
            sig, bak, ovr = q.mos(x)
            timbre = float(q.embed(x) @ ref_emb)
            words = similarity(expected, transcribe(rec, x))
            totals.setdefault(stage, []).append((sig, bak, ovr, timbre, words))
            line.append(f"{stage[:8]:>8} {sig:.2f}/{bak:.2f}/{ovr:.2f} т{timbre:.2f} р{words:.2f}")
        mark = " <- эталон" if f == ref_file else ""
        print(f"{f.stem[:34]:<34} | " + " | ".join(line) + mark)

    print("\nСреднее по вариантам: SIG (искажения) / BAK (фон) / OVRL, тембр, разборчивость")
    for stage, rows in totals.items():
        a = np.mean(rows, axis=0)
        print(f"  {stage:<10} {a[0]:.2f} / {a[1]:.2f} / {a[2]:.2f}   тембр {a[3]:.2f}   разборчивость {a[4]:.2f}")


if __name__ == "__main__":
    main()
