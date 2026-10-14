"""Тексты для обучения голоса Piper: фразы из корпуса Tatoeba (CC BY 2.0 FR,
out/piper/src/rus_sentences.tsv.bz2) + реплики в духе Джарвиса
(jarvis_lines.txt). Отбор: только кириллица и обычная пунктуация (без цифр
и латиницы — их модели читают по-разному), разумная длина, без повторов;
имён «Том» и «Мэри» в Tatoeba очень много — их доля ограничена.

Результат — out/piper/sentences.txt (по фразе в строке).
Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\piper\\corpus.py [--count 700]
"""

from __future__ import annotations

import argparse
import bz2
import random
import re
from pathlib import Path

HERE = Path(__file__).resolve().parent
OUT = HERE.parent / "out" / "piper"
ALLOWED = re.compile(r"^[А-Яа-яЁё][А-Яа-яЁё ,.!?:;—\-«»\"]+[.!?]$")
NAMES = re.compile(r"\b(Том|Тома|Тому|Томом|Мэри|Мэр)\b")


def jarvis_lines() -> list[str]:
    lines = (HERE / "jarvis_lines.txt").read_text(encoding="utf-8").splitlines()
    return [s.strip() for s in lines if s.strip() and not s.startswith("#")]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--count", type=int, default=700, help="сколько фраз из Tatoeba")
    args = parser.parse_args()

    seen: set[str] = set()
    plain, named = [], []
    with bz2.open(OUT / "src" / "rus_sentences.tsv.bz2", "rt", encoding="utf-8") as f:
        for line in f:
            parts = line.rstrip("\n").split("\t")
            if len(parts) < 3:
                continue
            text = " ".join(parts[2].split())
            words = len(text.split())
            if not (35 <= len(text) <= 150 and 5 <= words <= 22 and ALLOWED.match(text)):
                continue
            key = text.lower()
            if key in seen:
                continue
            seen.add(key)
            (named if NAMES.search(text) else plain).append(text)

    rng = random.Random(14)
    rng.shuffle(plain)
    rng.shuffle(named)
    n_named = args.count // 10
    picked = plain[: args.count - n_named] + named[:n_named]
    rng.shuffle(picked)
    sentences = jarvis_lines() + picked
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "sentences.txt").write_text("\n".join(sentences) + "\n", encoding="utf-8")
    print(f"подходящих в Tatoeba: {len(plain) + len(named)}; выбрано {len(picked)} + {len(sentences) - len(picked)} реплик")


if __name__ == "__main__":
    main()
