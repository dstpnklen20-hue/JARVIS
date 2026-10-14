"""Чистый голос dmitri, опущенный до высоты Джарвиса, — варианты на слух.

Замер (2026-10-14): средняя высота у Джарвиса из фильма 118 Гц (диапазон
94–155), у dmitri — 180 Гц (147–209); форманты почти совпадают (F3 у
Джарвиса ниже на ~9 %). Поэтому сдвигается высота (PSOLA — темп и тембр
остаются естественными), немного форманты («размер» голоса) и ширина
интонации. Метод — «Change gender» из Praat (parselmouth).

Страница: out/lower_dmitri.html (фразы dmitri со страницы piper_listen).
Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\piper\\lower_dmitri.py
"""

from __future__ import annotations

import html
import shutil
import sys
import time
from pathlib import Path

import parselmouth
from parselmouth.praat import call

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from verify import OUT  # noqa: E402

SRC = OUT / "piper_listen"
DEST = OUT / "lower_dmitri"
# (имя, средняя высота Гц, сдвиг формант, ширина интонации)
VARIANTS = [
    ("A", 140, 1.00, 1.0),
    ("B", 125, 0.97, 1.1),
    ("C", 118, 0.95, 1.2),
    ("D", 118, 0.92, 1.2),
    ("E", 108, 0.95, 1.2),
]
NOTES = {
    "A": "чуть ниже, без изменения тембра",
    "B": "ниже, голос чуть «больше»",
    "C": "как у Джарвиса (118 Гц), голос «больше», живее интонация",
    "D": "как C, голос ещё «больше»",
    "E": "ниже Джарвиса (108 Гц)",
}


def lower(src: Path, dst: Path, median: float, formant: float, range_factor: float) -> float:
    sound = parselmouth.Sound(str(src))
    t = time.time()
    out = call(sound, "Change gender", 75, 300, formant, median, range_factor, 1.0)
    secs = time.time() - t
    out.save(str(dst), "WAV")
    return secs


def main() -> None:
    DEST.mkdir(parents=True, exist_ok=True)
    from listen_piper import SENTENCES  # noqa: PLC0415

    blocks = []
    for i, text in enumerate(SENTENCES):
        src = SRC / f"{i}__dmitri.wav"
        shutil.copy(src, DEST / f"{i}__dmitri.wav")
        rows = [("dmitri", "как сейчас", f"{i}__dmitri.wav", None)]
        for name, median, formant, rng in VARIANTS:
            file = f"{i}__{name}.wav"
            secs = lower(src, DEST / file, median, formant, rng)
            rows.append((name, NOTES[name], file, secs))
        cells = "".join(
            f'<div class="row"><span class="name">{html.escape(n)}</span>'
            f'<span class="t">{html.escape(note)}{"" if s is None else f" · {s * 1000:.0f} мс"}</span>'
            f'<audio controls preload="none" src="lower_dmitri/{f}"></audio></div>'
            for n, note, f, s in rows
        )
        blocks.append(f"<section><h2>{html.escape(text)}</h2>{cells}</section>")

    refs = sorted((OUT / "clean").glob("*.wav"))[:3]
    ref_rows = []
    for k, ref in enumerate(refs):
        shutil.copy(ref, DEST / f"ref_{k}.wav")
        title = ref.stem.split("__")[-1]
        ref_rows.append(
            f'<div class="row"><span class="name">Джарвис</span><span class="t">{html.escape(title)}</span>'
            f'<audio controls preload="none" src="lower_dmitri/ref_{k}.wav"></audio></div>'
        )
    blocks.insert(0, f"<section><h2>Образец: Джарвис из фильма</h2>{''.join(ref_rows)}</section>")

    page = f"""<!doctype html><meta charset="utf-8"><title>dmitri голосом Джарвиса</title>
<style>
body{{background:#05090e;color:#d6e4ee;font:15px/1.45 "Segoe UI",sans-serif;max-width:900px;margin:24px auto;padding:0 16px}}
h1{{font:600 20px Bahnschrift,sans-serif;letter-spacing:.06em;color:#7fc4ff}} p{{color:#6c7f90}}
h2{{font-size:15px;font-weight:600;margin:0 0 10px}}
section{{background:#0b131b;border:1px solid #182532;border-left:2px solid #1e8cf0;padding:14px 16px;margin:14px 0}}
.row{{display:grid;grid-template-columns:90px 1fr 280px;gap:8px;align-items:center;padding:3px 0}}
.t{{color:#6c7f90;font-size:13px}} audio{{height:32px;width:280px}}
</style>
<h1>DMITRI ГОЛОСОМ ДЖАРВИСА</h1>
<p>Чистый голос dmitri, опущенный до высоты Джарвиса (118 Гц против 180 у dmitri). A–E — разная
высота и «размер» голоса. Мс — сколько занимает обработка фразы на процессоре.</p>
{''.join(blocks)}"""
    (OUT / "lower_dmitri.html").write_text(page, encoding="utf-8")
    print("Страница:", OUT / "lower_dmitri.html")


if __name__ == "__main__":
    main()
