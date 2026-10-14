"""Хрип и запинки обученного голоса — от настроек синтеза или от модели?

У Piper (VITS) при озвучке две «случайности»: noise_scale — шум в самом
голосе (выше — живее, но с хрипом и придыханием), noise_scale_w — разброс
длительности звуков (выше — живее ритм, но запинки, похожие на заикание).
По умолчанию 0.667 и 0.8. Здесь — те же фразы с меньшими значениями.

Страница: out/noise_test.html. Голос — models/vits-piper-ru_RU-<имя>
(его кладёт listen_piper.py).
Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\piper\\noise_test.py [jarvis-e60]
"""

from __future__ import annotations

import html
import shutil
import sys
from pathlib import Path

import numpy as np
import sherpa_onnx
import soundfile as sf

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from listen_piper import MODELS, SENTENCES  # noqa: E402
from verify import OUT  # noqa: E402

DEST = OUT / "noise_test"
# (имя, noise_scale, noise_scale_w)
SETTINGS = [
    ("как было", 0.667, 0.8),
    ("меньше шума", 0.4, 0.8),
    ("меньше запинок", 0.667, 0.4),
    ("меньше того и другого", 0.4, 0.4),
    ("минимум", 0.2, 0.2),
]


def synth(voice_dir: Path, noise: float, noise_w: float) -> sherpa_onnx.OfflineTts:
    model = next(voice_dir.glob("*.onnx"))
    config = sherpa_onnx.OfflineTtsConfig(
        model=sherpa_onnx.OfflineTtsModelConfig(
            vits=sherpa_onnx.OfflineTtsVitsModelConfig(
                model=str(model),
                tokens=str(voice_dir / "tokens.txt"),
                data_dir=str(voice_dir / "espeak-ng-data"),
                noise_scale=noise,
                noise_scale_w=noise_w,
            ),
            num_threads=2,
        ),
    )
    return sherpa_onnx.OfflineTts(config)


def main() -> None:
    name = sys.argv[1] if len(sys.argv) > 1 else "jarvis-e60"
    voice_dir = MODELS / f"vits-piper-ru_RU-{name}"
    DEST.mkdir(parents=True, exist_ok=True)
    rows: dict[int, list[str]] = {i: [] for i in range(len(SENTENCES))}
    for k, (label, noise, noise_w) in enumerate(SETTINGS):
        tts = synth(voice_dir, noise, noise_w)
        for i, text in enumerate(SENTENCES):
            audio = tts.generate(text, sid=0, speed=1.0)
            file = f"{i}__{k}.wav"
            sf.write(DEST / file, np.array(audio.samples, dtype=np.float32), audio.sample_rate)
            rows[i].append(
                f'<div class="row"><span class="name">{html.escape(label)}</span>'
                f'<span class="t">шум {noise} · ритм {noise_w}</span>'
                f'<audio controls preload="none" src="noise_test/{file}"></audio></div>'
            )
    refs = sorted((OUT / "clean").glob("*.wav"))[:2]
    ref_rows = []
    for k, ref in enumerate(refs):
        shutil.copy(ref, DEST / f"ref_{k}.wav")
        ref_rows.append(
            f'<div class="row"><span class="name">Джарвис</span>'
            f'<span class="t">{html.escape(ref.stem.split("__")[-1])}</span>'
            f'<audio controls preload="none" src="noise_test/ref_{k}.wav"></audio></div>'
        )
    blocks = [f"<section><h2>Образец: Джарвис из фильма</h2>{''.join(ref_rows)}</section>"]
    blocks += [
        f"<section><h2>{html.escape(SENTENCES[i])}</h2>{''.join(cells)}</section>" for i, cells in rows.items()
    ]
    page = f"""<!doctype html><meta charset="utf-8"><title>Хрип и запинки</title>
<style>
body{{background:#05090e;color:#d6e4ee;font:15px/1.45 "Segoe UI",sans-serif;max-width:900px;margin:24px auto;padding:0 16px}}
h1{{font:600 20px Bahnschrift,sans-serif;letter-spacing:.06em;color:#7fc4ff}} p{{color:#6c7f90}}
h2{{font-size:15px;font-weight:600;margin:0 0 10px}}
section{{background:#0b131b;border:1px solid #182532;border-left:2px solid #1e8cf0;padding:14px 16px;margin:14px 0}}
.row{{display:grid;grid-template-columns:190px 1fr 280px;gap:8px;align-items:center;padding:3px 0}}
.t{{color:#6c7f90;font-size:13px}} audio{{height:32px;width:280px}}
</style>
<h1>ГОЛОС {html.escape(name.upper())}: ХРИП И ЗАПИНКИ</h1>
<p>Один и тот же обученный голос с разными настройками синтеза. «Шум» — хрип и придыхание,
«ритм» — запинки. Меньше — чище и ровнее, но может стать монотоннее.</p>
{''.join(blocks)}"""
    (OUT / "noise_test.html").write_text(page, encoding="utf-8")
    print("Страница:", OUT / "noise_test.html")


if __name__ == "__main__":
    main()
