"""Озвучка набора для обучения Piper голосом Джарвиса (ESpeech/F5).

Каждая фраза out/piper/sentences.txt → out/piper/raw/NNNN.wav (24 кГц, как
выдаёт модель). Уже озвученные пропускаются — можно прервать и продолжить.
Проверка разборчивости и подготовка к обучению — prepare.py.

Запуск: tools\\voice\\.venv-f5\\Scripts\\python.exe tools\\voice\\piper\\synth.py --ref dry
Второй дубль в обычном темпе: ... synth.py --ref dry --speed 1.0 --out raw_b --seed 10000
(prepare.py выберет из дублей лучший).
"""

from __future__ import annotations

import argparse
import sys
import time
from pathlib import Path

import soundfile as sf
import torch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from clone_f5 import (  # noqa: E402
    F5,
    MODEL_CFG,
    MODEL_FILE,
    MODEL_REPO,
    OUT,
    REFERENCE_TEXT,
    VOCAB_REPO,
    Stresser,
    with_pauses,
)
from f5_tts.infer.utils_infer import infer_process, load_model, load_vocoder, preprocess_ref_audio_text  # noqa: E402
from f5_tts.model import DiT  # noqa: E402
from huggingface_hub import hf_hub_download  # noqa: E402

PIPER = OUT / "piper"


def reference(name: str) -> tuple[str, str]:
    import csv

    with open(OUT / "report.csv", encoding="utf-8-sig") as f:
        real = {r["file"]: r["name"] for r in csv.DictReader(f, delimiter=";")}
    return {
        "dry": (str(OUT / "clean" / f"{real['Запрос выполнен сэр']}.wav"), REFERENCE_TEXT),
        "clean": (str(F5 / "failed_3" / "ref_short_3.wav"), "Что-то пошло не так, сэр."),
    }[name]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--ref", choices=["clean", "dry"], default="clean")
    parser.add_argument("--steps", type=int, default=64)
    parser.add_argument("--speed", type=float, default=0.9)
    parser.add_argument("--out", default="raw", help="папка дублей в out/piper (второй дубль — raw_b)")
    parser.add_argument("--seed", type=int, default=0, help="сдвиг зерна: другой дубль той же фразы")
    args = parser.parse_args()

    sentences = (PIPER / "sentences.txt").read_text(encoding="utf-8").splitlines()
    raw = PIPER / args.out
    raw.mkdir(parents=True, exist_ok=True)
    todo = [(i, s) for i, s in enumerate(sentences) if s and not (raw / f"{i:04d}.wav").exists()]
    print(f"фраз {len(sentences)}, озвучить {len(todo)}", flush=True)
    if not todo:
        return

    stress = Stresser()
    model = load_model(
        DiT, MODEL_CFG, hf_hub_download(MODEL_REPO, MODEL_FILE),
        vocab_file=hf_hub_download(VOCAB_REPO, "vocab.txt"),
    )
    vocoder = load_vocoder()
    wav, text = reference(args.ref)
    ref_audio, ref_text = preprocess_ref_audio_text(wav, stress(text), show_info=lambda *_: None)

    started = time.time()
    audio_secs = 0.0
    for n, (i, sentence) in enumerate(todo, 1):
        torch.manual_seed(i + args.seed)
        wave, rate, _ = infer_process(
            ref_audio, ref_text, stress(with_pauses(sentence)), model, vocoder,
            nfe_step=args.steps, speed=args.speed, show_info=lambda *_: None, progress=None,
        )
        sf.write(raw / f"{i:04d}.wav", wave, rate)
        audio_secs += len(wave) / rate
        if n % 25 == 0 or n == len(todo):
            spent = time.time() - started
            left = spent / n * (len(todo) - n)
            print(f"{n}/{len(todo)}: речи {audio_secs / 60:.1f} мин, RTF {spent / audio_secs:.2f}, осталось ~{left / 60:.0f} мин", flush=True)


if __name__ == "__main__":
    main()
