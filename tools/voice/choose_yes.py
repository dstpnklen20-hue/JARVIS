"""Страница выбора «Да, сэр»: out/да_сэр.html — текущий вариант пакета и
лучшие дубли (out/f5_yes), где распознаются и «да», и «сэр». Варианты
обработаны так же, как в пакете (polish.py), — слышно ровно то, что будет
звучать. Выбор копируется строкой в чат.

Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\choose_yes.py
"""

from __future__ import annotations

import csv
import html
import shutil
from pathlib import Path

import numpy as np
import pyloudnorm
import soundfile as sf

from clean import TARGET_RATE, finalize
from polish import polish
from quality import Quality
from verify import OUT, load, recognizer, transcribe

ROOT = Path(__file__).resolve().parents[2]
TOP = 10
CHOICES = OUT / "да_сэр_выбор"


def main() -> None:
    q, rec = Quality(), recognizer()
    with open(OUT / "report.csv", encoding="utf-8-sig") as f:
        etalon = next(r["name"] for r in csv.DictReader(f, delimiter=";") if r["file"] == "Запрос выполнен сэр")
    etalon_path = OUT / "clean" / f"{etalon}.wav"
    emb = q.embed(load(etalon_path))
    target = pyloudnorm.Meter(TARGET_RATE, block_size=0.2).integrated_loudness(sf.read(etalon_path)[0])

    scored = []
    for wav in sorted((OUT / "f5_yes").rglob("*.wav")):
        x = load(wav)
        x = x / (np.abs(x).max() or 1.0)
        words = transcribe(rec, x).split()
        if "да" not in words or not any(w.startswith(("сэ", "се")) for w in words):
            continue
        _, _, ovr = q.mos(x)
        scored.append((float(q.embed(x) @ emb) + 0.15 * ovr, wav))
    scored.sort(key=lambda s: -s[0])

    if CHOICES.exists():
        shutil.rmtree(CHOICES)
    CHOICES.mkdir(parents=True)
    current = ROOT / "sounds" / "jarvis" / "wake_1.wav"
    shutil.copyfile(current, CHOICES / "0.wav")
    items = [("0", "Сейчас в пакете", "0.wav")]
    for i, (_, wav) in enumerate(scored[:TOP], 1):
        dst = CHOICES / f"{i}.wav"
        finalize(wav, dst)
        polish(dst, target)
        items.append((str(i), f"Вариант {i} · {wav.parent.name.replace('wake_1_', 'темп ')} · {wav.stem}", f"{i}.wav"))

    cards = "".join(
        f"""<div class="card" data-n="{n}">
  <div class="title">{html.escape(label)}</div>
  <audio controls preload="none" src="да_сэр_выбор/{src}"></audio>
  <button>Выбрать</button>
</div>"""
        for n, label, src in items
    )
    page = f"""<!doctype html>
<html lang="ru"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Выбор «Да, сэр»</title>
<style>
:root {{ --bg:#f6f7f9; --fg:#1b1f24; --muted:#5b6572; --card:#fff; --line:#dde2e8; --accent:#2f6fec; }}
@media (prefers-color-scheme: dark) {{ :root {{ --bg:#14171b; --fg:#e7eaee; --muted:#9aa4b1; --card:#1c2026; --line:#2c323a; --accent:#6b9bff; }} }}
body {{ margin:0 auto; padding:16px; max-width:760px; background:var(--bg); color:var(--fg); font:15px/1.45 system-ui, sans-serif; }}
h1 {{ font-size:22px; }}
.card {{ display:flex; flex-wrap:wrap; align-items:center; gap:10px; background:var(--card); border:1px solid var(--line); border-radius:10px; padding:10px 12px; margin:10px 0; }}
.card.on {{ border-color:var(--accent); box-shadow:0 0 0 2px var(--accent) inset; }}
.title {{ flex:1 1 220px; color:var(--muted); }}
audio {{ height:32px; max-width:100%; }}
button {{ border:1px solid var(--line); background:transparent; color:var(--fg); border-radius:6px; padding:6px 12px; cursor:pointer; }}
.card.on button {{ background:var(--accent); color:#fff; border-color:var(--accent); }}
.bar {{ position:sticky; bottom:0; background:var(--bg); padding:10px 0; display:flex; gap:10px; align-items:center; flex-wrap:wrap; }}
#out {{ flex:1 1 240px; padding:8px; border:1px solid var(--line); border-radius:8px; background:var(--card); color:var(--fg); font:14px monospace; }}
</style></head><body>
<h1>Выбор «Да, сэр»</h1>
<p>Все варианты обработаны так же, как в пакете. Послушайте и нажмите «Выбрать» у лучшего, затем скопируйте строку внизу в чат.</p>
<div class="card"><div class="title">Эталон — «Запрос выполнен, сэр»</div><audio controls preload="none" src="clean/{html.escape(etalon)}.wav"></audio></div>
{cards}
<div class="bar"><input id="out" readonly value="Ничего не выбрано"><button id="copy">Скопировать</button></div>
<script>
const cards = document.querySelectorAll('.card[data-n]');
const out = document.getElementById('out');
cards.forEach(c => c.querySelector('button').addEventListener('click', () => {{
  cards.forEach(x => x.classList.toggle('on', x === c));
  out.value = 'Да, сэр — выбран вариант ' + c.dataset.n;
}}));
document.getElementById('copy').addEventListener('click', () => {{
  out.select();
  (navigator.clipboard ? navigator.clipboard.writeText(out.value) : Promise.reject()).catch(() => document.execCommand('copy'));
}});
</script></body></html>"""
    (OUT / "да_сэр.html").write_text(page, encoding="utf-8")
    print(f"Страница: {OUT / 'да_сэр.html'} — {len(items)} вариантов")


if __name__ == "__main__":
    main()
