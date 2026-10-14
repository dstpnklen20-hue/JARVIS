"""Голос для ответов нейросети: сравнение на слух и замер скорости.

Типичные фразы ответов озвучиваются нынешним синтезом (Piper, через
`jarvis.exe --say`) и голосом Джарвиса (ESpeech/F5) с разным числом шагов
(меньше шагов — быстрее, но «металлистее»). Для каждого дубля замеряется
время синтеза одного предложения — столько ждёт ответ перед звуком.

Результат — out/llm_voice/ и страница out/llm_voice.html (выбор лучшего
варианта у каждой фразы, текст оценок — скопировать в чат).

Запуск: tools\\voice\\.venv-f5\\Scripts\\python.exe tools\\voice\\llm_voice_test.py
"""

from __future__ import annotations

import csv
import html
import json
import subprocess
import time

import numpy as np
import soundfile as sf
import torch

from clone_f5 import (
    F5,
    MODEL_CFG,
    MODEL_FILE,
    MODEL_REPO,
    OUT,
    REFERENCE_TEXT,
    ROOT,
    SPEED,
    TAIL_SECS,
    VOCAB_REPO,
    Stresser,
    with_pauses,
)
from f5_tts.infer.utils_infer import infer_process, load_model, load_vocoder, preprocess_ref_audio_text
from f5_tts.model import DiT
from huggingface_hub import hf_hub_download

SENTENCES = {
    "stark": "Тони Старк — гениальный изобретатель, промышленник и глава Старк Индастриз, сэр.",
    "weather": "Сейчас в Самаре плюс двенадцать градусов, облачно, к вечеру возможен дождь.",
    "reactor": "Ядерный реактор нагревает воду энергией деления урана, а пар вращает турбину генератора.",
    "volume": "Готово, сэр. Громкость уменьшена до сорока процентов.",
    "latin": "Открываю YouTube, сэр.",
}
STEPS = (16, 32)
DEST = OUT / "llm_voice"


def refs() -> dict[str, tuple[str, str]]:
    """Образцы голоса, которые вы отметили «эталоном» в раунде 3."""
    with open(OUT / "report.csv", encoding="utf-8-sig") as f:
        real = {r["file"]: r["name"] for r in csv.DictReader(f, delimiter=";")}
    return {
        "dry": (str(OUT / "clean" / f"{real['Запрос выполнен сэр']}.wav"), REFERENCE_TEXT),
        "clean": (str(F5 / "failed_3" / "ref_short_3.wav"), "Что-то пошло не так, сэр."),
    }


def piper(sid: str, text: str) -> dict:
    target = DEST / f"{sid}__piper.wav"
    started = time.time()
    subprocess.run(
        [str(ROOT / "target" / "release" / "jarvis.exe"), "--say", text, "--out", str(target)],
        check=True,
        capture_output=True,
        timeout=60,
    )
    # Время процесса включает загрузку модели — в Джарвисе она уже загружена,
    # синтез там — около 45 мс на секунду речи (DEVELOPMENT.md).
    secs = len(sf.read(target)[0]) / sf.info(target).samplerate
    return {"file": target.name, "variant": "piper", "synth": round(secs * 0.045, 2), "audio": round(secs, 2),
            "wall": round(time.time() - started, 2)}


def main() -> None:
    DEST.mkdir(parents=True, exist_ok=True)
    stress = Stresser()
    model = load_model(
        DiT, MODEL_CFG, hf_hub_download(MODEL_REPO, MODEL_FILE),
        vocab_file=hf_hub_download(VOCAB_REPO, "vocab.txt"),
    )
    vocoder = load_vocoder()
    prepared = {
        name: preprocess_ref_audio_text(wav, stress(text), show_info=lambda *_: None)
        for name, (wav, text) in refs().items()
    }
    # Прогрев: первый проход медленнее (так же будет и в Джарвисе при запуске).
    ref_audio, ref_text = prepared["dry"]
    infer_process(ref_audio, ref_text, "Пр+оба.", model, vocoder, nfe_step=16, speed=SPEED,
                  show_info=lambda *_: None, progress=None)

    results: dict[str, list[dict]] = {}
    for sid, text in SENTENCES.items():
        rows = [piper(sid, text)]
        t = time.time()
        marked = stress(with_pauses(text))
        stress_secs = time.time() - t
        for ref_name, (ref_audio, ref_text) in prepared.items():
            for steps in STEPS:
                torch.manual_seed(1)
                if torch.cuda.is_available():
                    torch.cuda.synchronize()
                t = time.time()
                wave, rate, _ = infer_process(ref_audio, ref_text, marked, model, vocoder, nfe_step=steps,
                                              speed=SPEED, show_info=lambda *_: None, progress=None)
                if torch.cuda.is_available():
                    torch.cuda.synchronize()
                synth = time.time() - t + stress_secs
                wave = np.concatenate([wave, np.zeros(int(TAIL_SECS * rate), dtype=wave.dtype)])
                name = f"{sid}__{ref_name}_{steps}.wav"
                sf.write(DEST / name, wave, rate)
                rows.append({"file": name, "variant": f"{ref_name}_{steps}", "synth": round(synth, 2),
                             "audio": round(len(wave) / rate, 2)})
        results[sid] = rows
        print(sid, marked, *(f"{r['variant']}={r['synth']}s" for r in rows), flush=True)

    (DEST / "timings.json").write_text(json.dumps(results, ensure_ascii=False, indent=1), encoding="utf-8")
    write_page(results)


LABELS = {
    "piper": "Сейчас (Piper, голос dmitri)",
    "dry_16": "Джарвис, образец «Запрос выполнен», 16 шагов",
    "dry_32": "Джарвис, образец «Запрос выполнен», 32 шага",
    "clean_16": "Джарвис, образец «Что-то пошло не так», 16 шагов",
    "clean_32": "Джарвис, образец «Что-то пошло не так», 32 шага",
}


def write_page(results: dict[str, list[dict]]) -> None:
    blocks = []
    for sid, rows in results.items():
        items = "".join(
            f'<label class="row"><input type="radio" name="{sid}" value="{r["variant"]}">'
            f'<span class="name">{html.escape(LABELS.get(r["variant"], r["variant"]))}</span>'
            f'<span class="t">{r["synth"]:.2f} с синтеза · {r["audio"]:.1f} с речи</span>'
            f'<audio controls preload="none" src="llm_voice/{r["file"]}"></audio></label>'
            for r in rows
        )
        blocks.append(
            f'<section><h2>{html.escape(SENTENCES[sid])}</h2>{items}'
            f'<input class="note" data-id="{sid}" placeholder="замечание (ударение, слово, интонация)"></section>'
        )
    page = f"""<!doctype html><meta charset="utf-8"><title>Голос ответов нейросети</title>
<style>
body{{background:#05090e;color:#d6e4ee;font:15px/1.45 "Segoe UI",sans-serif;max-width:880px;margin:24px auto;padding:0 16px}}
h1{{font:600 20px Bahnschrift,sans-serif;letter-spacing:.06em;color:#7fc4ff}}
p{{color:#6c7f90}} h2{{font-size:15px;font-weight:600;margin:0 0 10px}}
section{{background:#0b131b;border:1px solid #182532;border-left:2px solid #1e8cf0;padding:14px 16px;margin:14px 0}}
.row{{display:grid;grid-template-columns:20px 1fr 190px 260px;gap:8px;align-items:center;padding:3px 0}}
.t{{color:#6c7f90;font-size:13px}} audio{{height:32px;width:260px}}
.note,textarea{{width:100%;box-sizing:border-box;background:#05090e;color:#d6e4ee;border:1px solid #182532;padding:6px;margin-top:8px}}
button{{background:#1e8cf0;color:#fff;border:0;padding:8px 14px;margin-top:8px;cursor:pointer}}
</style>
<h1>ГОЛОС ОТВЕТОВ НЕЙРОСЕТИ</h1>
<p>У каждой фразы отметьте вариант, который нравится больше (по голосу и по тому, терпимо ли ожидание:
«с синтеза» — сколько ответ ждёт перед звуком на одно предложение, сверх времени самой нейросети).
Внизу соберётся текст — скопируйте его в чат.</p>
{''.join(blocks)}
<textarea id="out" rows="8" readonly></textarea>
<button onclick="navigator.clipboard.writeText(document.getElementById('out').value)">Скопировать</button>
<script>
function upd(){{
  const lines=[];
  document.querySelectorAll('section').forEach(s=>{{
    const r=s.querySelector('input[type=radio]:checked'); const n=s.querySelector('.note');
    if(r||n.value) lines.push(n.dataset.id+': '+(r?r.value:'—')+(n.value?' — '+n.value:''));
  }});
  document.getElementById('out').value=lines.join('\\n');
}}
document.addEventListener('input',upd); document.addEventListener('change',upd);
</script>"""
    (OUT / "llm_voice.html").write_text(page, encoding="utf-8")
    print("Страница:", OUT / "llm_voice.html")


if __name__ == "__main__":
    main()
