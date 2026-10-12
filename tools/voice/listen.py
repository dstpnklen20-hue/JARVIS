"""Страница прослушивания: out/listen.html — эталон, все дубли каждой фразы
(ESpeech и Chatterbox) с оценками. Открывается в браузере как локальный
файл, никуда не отправляется. Оценки собираются в текст внизу страницы —
его достаточно скопировать в чат.

Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\listen.py
"""

from __future__ import annotations

import csv
import html
import re
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "tools" / "voice" / "out"
# Текущий раунд (дубли и итог <раунд>_best) и прошлый — для сравнения.
CURRENT = "f5v2"
PREVIOUS = "f5"
REFERENCE = "Запрос выполнен сэр"


def auto_scores() -> dict[str, dict[str, str]]:
    """Выбранный дубль каждой фразы по отчётам pick_tts: {id: {раунд: дубль}}."""
    picks: dict[str, dict[str, str]] = {}
    for src in (CURRENT, PREVIOUS):
        report = OUT / f"{src}_report.csv"
        if report.exists():
            with open(report, encoding="utf-8-sig") as f:
                for row in csv.DictReader(f, delimiter=";"):
                    picks.setdefault(row["id"], {})[src] = row["take"]
    # Для раунда 1 — ваш лучший по оценкам дубль (data/оценки.txt), если есть.
    ratings = ROOT / "data" / "оценки.txt"
    if ratings.exists():
        rank = {"эталон": 2, "неплохо": 1}
        best: dict[str, tuple[int, str]] = {}
        for line in ratings.read_text(encoding="utf-8").splitlines():
            m = re.match(rf"(\w+):{PREVIOUS}/(\S+)\s*=\s*(\S+)", line.strip())
            if m and m.group(3) in rank and rank[m.group(3)] > best.get(m.group(1), (0, ""))[0]:
                best[m.group(1)] = (rank[m.group(3)], m.group(2))
        for pid, (_, take) in best.items():
            picks.setdefault(pid, {})[PREVIOUS] = take
    return picks


def main() -> None:
    with open(OUT / "report.csv", encoding="utf-8-sig") as f:
        ref_name = next(r["name"] for r in csv.DictReader(f, delimiter=";") if r["file"] == REFERENCE)
    picks = auto_scores()
    phrases: dict[str, dict] = {}
    current = OUT / CURRENT
    for d in sorted(p for p in current.iterdir() if p.is_dir()):
        entry = phrases.setdefault(d.name, {"text": (d / "text.txt").read_text(encoding="utf-8"), "takes": []})
        stressed = d / "stressed.txt"
        if stressed.exists():
            entry["stressed"] = stressed.read_text(encoding="utf-8")
        # Итог раунда — выбранный и обработанный (громкость, звонкость, хвост).
        best = OUT / f"{CURRENT}_best" / f"{d.name}.wav"
        if best.exists():
            take = picks.get(d.name, {}).get(CURRENT, "")
            entry["takes"].append(
                {
                    "key": f"{d.name}:итог/{take}",
                    "src": f"{CURRENT}_best/{d.name}.wav",
                    "label": f"Итог раунда 2 · {take} · обработан",
                    "auto": True,
                }
            )
        # Для сравнения — что было в раунде 1.
        prev = picks.get(d.name, {}).get(PREVIOUS)
        if prev:
            entry["takes"].append(
                {
                    "key": f"{d.name}:раунд1/{prev}",
                    "src": f"{PREVIOUS}/{d.name}/{prev}.wav",
                    "label": f"Раунд 1 · {prev} (ваш лучший / для сравнения)",
                    "auto": True,
                }
            )
        for wav in sorted(d.glob("*.wav")):
            entry["takes"].append(
                {
                    "key": f"{d.name}:{CURRENT}/{wav.stem}",
                    "src": f"{CURRENT}/{d.name}/{wav.name}",
                    "label": f"дубль {wav.stem} (сырой)",
                    "auto": False,
                }
            )

    def take_html(t: dict) -> str:
        return f"""<div class="take{' auto' if t['auto'] else ''}" data-key="{html.escape(t['key'])}">
  <span class="label">{html.escape(t['label'])}{' · выбор по цифрам' if t['auto'] else ''}</span>
  <audio controls preload="none" src="{html.escape(t['src'])}"></audio>
  <span class="rate">
    <button data-v="эталон">как эталон</button><button data-v="неплохо">неплохо</button><button data-v="плохо">плохо</button><button data-v="ударение" class="flag">ударение</button>
  </span>
</div>"""

    rows = []
    for pid, e in phrases.items():
        if not e["takes"]:
            continue
        # Сверху итог раунда и раунд 1 для сравнения, сырые дубли — под катом.
        top = [t for t in e["takes"] if t["auto"]]
        rest = [t for t in e["takes"] if not t["auto"]]
        more = (
            f'<details><summary>все дубли раунда 2, без обработки ({len(rest)})</summary>{"".join(map(take_html, rest))}</details>'
            if rest
            else ""
        )
        stressed = f'<div class="stressed">ударения: {html.escape(e["stressed"])}</div>' if "stressed" in e else ""
        rows.append(
            f'<section><h2>{html.escape(pid)} — «{html.escape(e["text"])}»</h2>{stressed}'
            f'{"".join(map(take_html, top))}{more}</section>'
        )

    page = f"""<!doctype html>
<html lang="ru"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Голос Джарвиса</title>
<style>
:root {{ --bg:#f6f7f9; --fg:#1b1f24; --muted:#5b6572; --card:#fff; --line:#dde2e8; --accent:#2f6fec; --good:#1f9d55; --bad:#d64545; }}
@media (prefers-color-scheme: dark) {{ :root {{ --bg:#14171b; --fg:#e7eaee; --muted:#9aa4b1; --card:#1c2026; --line:#2c323a; --accent:#6b9bff; --good:#45c07a; --bad:#ff6b6b; }} }}
body {{ margin:0; padding:16px; background:var(--bg); color:var(--fg); font:15px/1.45 system-ui, sans-serif; max-width:980px; margin-inline:auto; }}
h1 {{ font-size:22px; }} h2 {{ font-size:16px; margin:0 0 6px; }}
section, .ref {{ background:var(--card); border:1px solid var(--line); border-radius:10px; padding:12px; margin:12px 0; }}
.take {{ display:flex; flex-wrap:wrap; align-items:center; gap:8px; padding:6px 0; border-top:1px solid var(--line); }}
.take.auto .label {{ color:var(--accent); }}
.label {{ min-width:230px; color:var(--muted); font-size:13px; }}
summary {{ cursor:pointer; color:var(--muted); margin:6px 0; }}
audio {{ height:32px; max-width:100%; }}
.stressed {{ color:var(--muted); font-size:13px; margin-bottom:4px; }}
button {{ border:1px solid var(--line); background:transparent; color:var(--fg); border-radius:6px; padding:4px 8px; cursor:pointer; font-size:13px; }}
button.on {{ background:var(--accent); color:#fff; border-color:var(--accent); }}
button.flag.on {{ background:var(--bad); border-color:var(--bad); }}
textarea {{ width:100%; min-height:160px; box-sizing:border-box; background:var(--card); color:var(--fg); border:1px solid var(--line); border-radius:8px; padding:8px; font:13px monospace; }}
.bar {{ position:sticky; bottom:0; background:var(--bg); padding:8px 0; }}
</style></head><body>
<h1>Голос Джарвиса — прослушивание</h1>
<p>Сначала послушайте эталон. У каждого дубля отметьте: как эталон / неплохо / плохо; «ударение» — если ударение не там.
Раунд 2. Сверху у каждой фразы — итог (выбран и обработан: громкость, звонкость, запас в конце), под ним для сравнения раунд 1, сырые дубли — под катом. Оценки собираются в поле внизу — скопируйте его в чат.</p>
<div class="ref"><b>Эталон — «Запрос выполнен, сэр»</b><br><audio controls src="clean/{html.escape(ref_name)}.wav"></audio></div>
{''.join(rows)}
<div class="bar"><b>Ваши оценки</b> <button id="copy">Скопировать</button><textarea id="out" readonly></textarea></div>
<script>
const state = {{}};
try {{ Object.assign(state, JSON.parse(localStorage.getItem('jarvis-voice-ratings-r2') || '{{}}')); }} catch (e) {{}}
function render() {{
  document.querySelectorAll('.take').forEach(t => {{
    const s = state[t.dataset.key] || {{}};
    t.querySelectorAll('button').forEach(b => b.classList.toggle('on', b.dataset.v === 'ударение' ? !!s.stress : s.rate === b.dataset.v));
  }});
  const lines = Object.entries(state).filter(([, s]) => s.rate || s.stress)
    .map(([k, s]) => k + ' = ' + [s.rate, s.stress ? 'ударение' : ''].filter(Boolean).join(', '));
  document.getElementById('out').value = lines.join('\\n');
  try {{ localStorage.setItem('jarvis-voice-ratings-r2', JSON.stringify(state)); }} catch (e) {{}}
}}
document.querySelectorAll('.take button').forEach(b => b.addEventListener('click', () => {{
  const key = b.closest('.take').dataset.key; const s = state[key] = state[key] || {{}};
  if (b.dataset.v === 'ударение') s.stress = !s.stress; else s.rate = s.rate === b.dataset.v ? '' : b.dataset.v;
  render();
}}));
document.getElementById('copy').addEventListener('click', () => {{
  const t = document.getElementById('out'); t.select();
  (navigator.clipboard ? navigator.clipboard.writeText(t.value) : Promise.reject()).catch(() => document.execCommand('copy'));
}});
render();
</script></body></html>"""
    (OUT / "listen.html").write_text(page, encoding="utf-8")
    total = sum(len(e["takes"]) for e in phrases.values())
    print(f"Страница: {OUT / 'listen.html'} — {len(phrases)} фраз, {total} дублей")


if __name__ == "__main__":
    main()
