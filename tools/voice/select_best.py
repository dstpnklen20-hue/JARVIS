"""Выбор лучшего варианта каждой реплики и итоговая нарезка.

Варианты: полная очистка (голос + шумоподавление), только шумоподавление,
исходник. Каждый распознаётся и сравнивается с текстом из имени файла.
Берётся самый чистый из тех, что распознаются не хуже лучшего (допуск 0.05):
очистка, съевшая слова, проигрывает менее чистому, но целому варианту.

Итог — tools/voice/out/clean/, отчёт — tools/voice/out/report.csv.
Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\select_best.py
"""

from __future__ import annotations

import csv
import difflib
import re
from pathlib import Path

import numpy as np
from num2words import num2words

from clean import finalize
from verify import OUT, SRC, load, recognizer, transcribe

TOLERANCE = 0.05
# Порядок предпочтения при равной разборчивости: чище — лучше.
VARIANTS = [
    ("полная очистка", "dry"),
    ("шумоподавление", "dry_orig"),
    ("исходник", None),
]
SKIP = ("Песня целиком",)


def reference(name: str) -> str:
    """Ожидаемый текст из имени файла: числа словами, без пометок."""
    text = re.sub(r"\((второй|второе)\)|=\)|\s2$", " ", name)
    text = re.sub(r"\d+", lambda m: " " + num2words(int(m.group()), lang="ru") + " ", text)
    return words(text)


def words(text: str) -> str:
    text = text.lower().replace("ё", "е")
    # «процент», «процента», «процентов» и знак % — одно и то же.
    return " ".join(w for w in re.findall(r"[а-я]+", text) if not w.startswith("процент"))


def similarity(expected: str, heard: str) -> float:
    return difflib.SequenceMatcher(None, expected, words(heard)).ratio()


def main() -> None:
    rec = recognizer()
    files = sorted(p for p in SRC.rglob("*.wav") if not any(s in p.stem for s in SKIP))
    (OUT / "clean").mkdir(parents=True, exist_ok=True)
    for old in (OUT / "clean").glob("*.wav"):
        old.unlink()
    rows = []
    for f in files:
        name = "__".join(f.relative_to(SRC).with_suffix("").parts)
        expected = reference(f.stem)
        scored = []
        for label, stage in VARIANTS:
            path = f if stage is None else OUT / "stems" / stage / f"{name}.wav"
            if not path.exists():
                continue
            x = load(path)
            heard = transcribe(rec, x / (np.abs(x).max() or 1.0))
            scored.append((label, path, heard, similarity(expected, heard)))
        best = max(s[3] for s in scored)
        label, path, heard, score = next(s for s in scored if s[3] >= best - TOLERANCE)
        secs = finalize(path, OUT / "clean" / f"{name}.wav")
        rows.append(
            {
                "file": name,
                "expected": expected,
                "chosen": label,
                "score": round(score, 2),
                "heard": heard,
                **{f"score_{l}": round(s, 2) for l, _, _, s in scored},
                "secs": round(secs, 1),
            }
        )
        print(f"{score:.2f} {label:<15} {secs:4.1f} с  {f.stem[:50]:<50} «{heard}»")

    with open(OUT / "report.csv", "w", newline="", encoding="utf-8-sig") as out:
        writer = csv.DictWriter(out, fieldnames=list(rows[0]), delimiter=";")
        writer.writeheader()
        writer.writerows(rows)
    good = [r for r in rows if r["score"] >= 0.7]
    by_choice = {l: sum(r["chosen"] == l for r in rows) for l, _ in VARIANTS}
    print(
        f"\nВыбрано: {by_choice}. Разборчивых (>= 0.7): {len(good)} из {len(rows)}, "
        f"{sum(r['secs'] for r in good) / 60:.1f} мин; всего {sum(r['secs'] for r in rows) / 60:.1f} мин"
    )


if __name__ == "__main__":
    main()
