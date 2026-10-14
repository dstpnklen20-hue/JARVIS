"""Голос Piper, обученный на Джарвисе, — в формат Джарвиса и на прослушивание.

1. Каждый out/piper/export/<имя>.onnx (выгрузка из обучения) получает
   метаданные sherpa-onnx (как у ru_RU-dmitri-medium: тип vits, piper, ru,
   espeak) и кладётся в models/vits-piper-ru_RU-<имя>/ вместе с tokens.txt
   и espeak-ng-data от dmitri (словарь фонем тот же — голос дообучен из dmitri).
2. Страница out/piper_listen.html: одни и те же фразы голосом dmitri и
   каждой выгрузки, со временем синтеза на процессоре.

Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\piper\\listen_piper.py
"""

from __future__ import annotations

import html
import json
import shutil
import sys
import time
from pathlib import Path

import numpy as np
import onnx
import sherpa_onnx
import soundfile as sf

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from verify import OUT, ROOT  # noqa: E402

MODELS = ROOT / "models"
DMITRI = MODELS / "vits-piper-ru_RU-dmitri-medium"
EXPORT = OUT / "piper" / "export"
DEST = OUT / "piper_listen"
# Фраз нет в обучающем наборе — слышно, как голос справляется с новым текстом.
SENTENCES = [
    "Добрый день, сэр. Я подготовил отчёт о состоянии системы.",
    "Марс — четвёртая планета от Солнца, её называют красной из-за оксида железа.",
    "Боюсь, ближайший свободный час у вас только после шести вечера.",
    "Конечно, сэр. Открываю календарь и переношу встречу на завтра.",
    "Чтобы приготовить хороший кофе, воду нужно нагреть примерно до девяноста градусов.",
]


def install(onnx_path: Path) -> Path:
    name = onnx_path.stem
    target = MODELS / f"vits-piper-ru_RU-{name}"
    target.mkdir(parents=True, exist_ok=True)
    model = onnx.load(str(onnx_path))
    meta = {p.key: p.value for p in onnx.load(str(DMITRI / "ru_RU-dmitri-medium.onnx")).metadata_props}
    del model.metadata_props[:]
    for key, value in meta.items():
        p = model.metadata_props.add()
        p.key, p.value = key, value
    onnx.save(model, str(target / f"ru_RU-{name}.onnx"))
    shutil.copy(DMITRI / "tokens.txt", target / "tokens.txt")
    if not (target / "espeak-ng-data").exists():
        shutil.copytree(DMITRI / "espeak-ng-data", target / "espeak-ng-data")
    return target


def synthesizer(voice_dir: Path) -> sherpa_onnx.OfflineTts:
    model = next(voice_dir.glob("*.onnx"))
    config = sherpa_onnx.OfflineTtsConfig(
        model=sherpa_onnx.OfflineTtsModelConfig(
            vits=sherpa_onnx.OfflineTtsVitsModelConfig(
                model=str(model),
                tokens=str(voice_dir / "tokens.txt"),
                data_dir=str(voice_dir / "espeak-ng-data"),
            ),
            num_threads=2,
        ),
    )
    return sherpa_onnx.OfflineTts(config)


def main() -> None:
    DEST.mkdir(parents=True, exist_ok=True)
    voices = {"dmitri": DMITRI}
    for exported in sorted(EXPORT.glob("*.onnx"), key=lambda p: p.stat().st_mtime):
        voices[exported.stem] = install(exported)

    rows: dict[str, list[tuple[str, str, float, float]]] = {}
    for name, voice_dir in voices.items():
        tts = synthesizer(voice_dir)
        tts.generate("Проба.", sid=0, speed=1.0)  # прогрев
        for i, text in enumerate(SENTENCES):
            t = time.time()
            audio = tts.generate(text, sid=0, speed=1.0)
            secs = time.time() - t
            samples = np.array(audio.samples, dtype=np.float32)
            file = f"{i}__{name}.wav"
            sf.write(DEST / file, samples, audio.sample_rate)
            rows.setdefault(text, []).append((name, file, secs, len(samples) / audio.sample_rate))
        print(name, "готов", flush=True)

    blocks = []
    for text, items in rows.items():
        cells = "".join(
            f'<div class="row"><span class="name">{html.escape(name)}</span>'
            f'<span class="t">{secs:.2f} с на процессоре · {dur:.1f} с речи</span>'
            f'<audio controls preload="none" src="piper_listen/{file}"></audio></div>'
            for name, file, secs, dur in items
        )
        blocks.append(f"<section><h2>{html.escape(text)}</h2>{cells}</section>")

    # Откуда хрип: из самих обучающих записей (их делала ESpeech) или от Piper.
    sentences = (OUT / "piper" / "sentences.txt").read_text(encoding="utf-8").splitlines()
    samples = []
    for i in (0, 4, 100, 300, 500):
        src = OUT / "piper" / "dataset" / "wavs" / f"{i:04d}.wav"
        if src.exists():
            shutil.copy(src, DEST / f"train_{i:04d}.wav")
            samples.append(
                f'<div class="row"><span class="name">запись {i}</span>'
                f'<span class="t">{html.escape(sentences[i])}</span>'
                f'<audio controls preload="none" src="piper_listen/train_{i:04d}.wav"></audio></div>'
            )
    for sid in ("stark", "weather"):
        liked = OUT / "llm_voice" / f"{sid}__dry_32.wav"
        if liked.exists():
            shutil.copy(liked, DEST / f"liked_{sid}.wav")
            samples.append(
                f'<div class="row"><span class="name">понравилось</span>'
                f'<span class="t">ESpeech, «Запрос выполнен», 32 шага</span>'
                f'<audio controls preload="none" src="piper_listen/liked_{sid}.wav"></audio></div>'
            )
    if samples:
        blocks.append(
            "<section><h2>Для сравнения: на чём учится голос</h2>"
            "<p>Обучающие записи (их озвучила ESpeech, 64 шага) и варианты, которые вам понравились "
            "на прошлой странице. Если хрип и заикание слышны уже здесь — дело в записях, а не в Piper.</p>"
            f"{''.join(samples)}</section>"
        )
    page = f"""<!doctype html><meta charset="utf-8"><title>Голос Джарвиса на Piper</title>
<style>
body{{background:#05090e;color:#d6e4ee;font:15px/1.45 "Segoe UI",sans-serif;max-width:860px;margin:24px auto;padding:0 16px}}
h1{{font:600 20px Bahnschrift,sans-serif;letter-spacing:.06em;color:#7fc4ff}} p{{color:#6c7f90}}
h2{{font-size:15px;font-weight:600;margin:0 0 10px}}
section{{background:#0b131b;border:1px solid #182532;border-left:2px solid #1e8cf0;padding:14px 16px;margin:14px 0}}
.row{{display:grid;grid-template-columns:150px 1fr 280px;gap:8px;align-items:center;padding:3px 0}}
.t{{color:#6c7f90;font-size:13px}} audio{{height:32px;width:280px}}
</style>
<h1>ГОЛОС ДЖАРВИСА НА PIPER — ПРОБА</h1>
<p>dmitri — как сейчас. Остальные — голос, дообученный на записях Джарвиса; число в имени — шаг
обучения (больше — дольше учился). Фраз нет в обучающем наборе. Работает на процессоре, без видеокарты.</p>
{''.join(blocks)}"""
    (OUT / "piper_listen.html").write_text(page, encoding="utf-8")
    (DEST / "voices.json").write_text(json.dumps(list(voices), ensure_ascii=False), encoding="utf-8")
    print("Страница:", OUT / "piper_listen.html")


if __name__ == "__main__":
    main()
