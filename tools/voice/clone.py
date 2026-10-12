"""Новые реплики голосом эталона: клонирование Chatterbox Multilingual (MIT).

Образцы голоса (out/tts/ref_*.wav):
- ref_short — только эталон «Запрос выполнен сэр»;
- ref_long  — эталон + реплики уровня «как эталон» с самым близким тембром
  (по out/report.csv), ~10 с: модель берёт до 10 с образца.
Каждая фраза генерируется TAKES раз с каждым образцом; лучший дубль
выбирает pick_tts.py (тембр, DNSMOS, разборчивость).

Запуск (отдельное окружение — свои версии transformers):
  tools\\voice\\.venv-tts\\Scripts\\python.exe tools\\voice\\clone.py
"""

from __future__ import annotations

import csv
import json
import time
from pathlib import Path

import numpy as np
import soundfile as sf
import torch
from chatterbox.mtl_tts import ChatterboxMultilingualTTS

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "tools" / "voice" / "out"
TTS = OUT / "tts"
REFERENCE = "Запрос выполнен сэр"
TAKES = 4
REF_LONG_SECS = 10.0

# id → текст. id = ответ звукового пакета + номер варианта.
PHRASES = {
    "wake_1": "Да, сэр.",
    "wake_2": "Слушаю, сэр.",
    "wake_3": "К вашим услугам, сэр.",
    "wake_4": "Всегда к вашим услугам, сэр.",
    "ok_1": "Готово, сэр.",
    "ok_2": "Выполнено.",
    "ok_3": "Есть.",
    "not_understood_1": "Простите, сэр, я не расслышал.",
    "not_understood_2": "Не понял, сэр.",
    "failed_1": "Не удалось, сэр.",
    "failed_2": "Боюсь, это невозможно, сэр.",
    "ask_1": "Сэр, разрешите выполнить?",
    "ask_2": "Разрешите, сэр?",
    "denied_1": "Как пожелаете, сэр.",
    "denied_2": "Как скажете, сэр.",
    # Динамическая фраза — оценить качество и скорость для синтеза на лету.
    "dynamic_1": "Сэр, команда «открой ютуб» откроет YouTube в браузере. Разрешить?",
}


def build_references() -> dict[str, Path]:
    with open(OUT / "report.csv", encoding="utf-8-sig") as f:
        rows = list(csv.DictReader(f, delimiter=";"))
    by_file = {r["file"]: r for r in rows}
    ref = by_file[REFERENCE]
    others = sorted(
        (r for r in rows if r["tier"] == "как эталон" and r["file"] != REFERENCE and float(r["words"]) >= 0.9),
        key=lambda r: -float(r["timbre"]),
    )
    TTS.mkdir(parents=True, exist_ok=True)
    rate = 24_000

    def read(name: str) -> np.ndarray:
        import librosa

        return librosa.load(OUT / "clean" / f"{name}.wav", sr=rate)[0]

    short = read(ref["name"])
    sf.write(TTS / "ref_short.wav", short, rate)

    parts, used = [short], [REFERENCE]
    gap = np.zeros(int(0.3 * rate), dtype=np.float32)
    for r in others:
        if sum(len(p) for p in parts) / rate >= REF_LONG_SECS:
            break
        parts += [gap, read(r["name"])]
        used.append(r["file"])
    sf.write(TTS / "ref_long.wav", np.concatenate(parts)[: int(REF_LONG_SECS * rate)], rate)
    (TTS / "references.json").write_text(json.dumps({"ref_long": used}, ensure_ascii=False, indent=2), encoding="utf-8")
    print("Образец ref_long:", "; ".join(used))
    return {"ref_short": TTS / "ref_short.wav", "ref_long": TTS / "ref_long.wav"}


def main() -> None:
    import argparse

    parser = argparse.ArgumentParser(description="Реплики голосом эталона")
    parser.add_argument("text", nargs="?", help="своя фраза (без неё — все фразы пакета)")
    parser.add_argument("--id", default="custom", help="имя папки в out/tts для своей фразы")
    parser.add_argument("--takes", type=int, default=TAKES, help="сколько дублей сгенерировать")
    parser.add_argument("--ref", choices=["ref_short", "ref_long", "both"], default="ref_short",
                        help="образец голоса (ref_short — только эталон, лучше по замерам)")
    args = parser.parse_args()
    phrases = {args.id: args.text} if args.text else PHRASES

    refs = build_references()
    if args.ref != "both":
        refs = {args.ref: refs[args.ref]}
    started = time.time()
    model = ChatterboxMultilingualTTS.from_pretrained(device=torch.device("cuda"))
    print(f"Модель загружена за {time.time() - started:.1f} с")
    timings = []
    for ref_name, ref_path in refs.items():
        model.prepare_conditionals(str(ref_path), exaggeration=0.5)
        for pid, text in phrases.items():
            out_dir = TTS / pid
            out_dir.mkdir(parents=True, exist_ok=True)
            (out_dir / "text.txt").write_text(text, encoding="utf-8")
            for take in range(1, args.takes + 1):
                target = out_dir / f"{ref_name}_{take}.wav"
                if target.exists():
                    continue
                torch.manual_seed(take)
                t = time.time()
                wav = model.generate(text, language_id="ru", exaggeration=0.5, cfg_weight=0.5, temperature=0.8)
                secs = time.time() - t
                audio = wav.squeeze(0).cpu().numpy()
                sf.write(target, audio, model.sr)
                timings.append((secs, len(audio) / model.sr))
            print(f"  {ref_name} {pid}: «{text}»", flush=True)
    if timings:
        synth = sum(t for t, _ in timings)
        audio = sum(a for _, a in timings)
        print(f"Синтез: {len(timings)} дублей, RTF {synth / audio:.2f} ({synth / len(timings):.2f} с на дубль)")


if __name__ == "__main__":
    main()
