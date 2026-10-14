"""Набор для обучения Piper из озвученных фраз (synth.py).

Для каждой фразы — дубли из out/piper/<raw>/NNNN.wav: распознавание
(zipformer, как у Джарвиса) и сравнение с текстом — дубли с перепутанными,
пропущенными словами или заиканием отбрасываются; из оставшихся берётся самый
чистый голос (DNSMOS SIG — хрип его снижает); самые хриплые фразы можно убрать
совсем (--drop-worst). Тишина по краям обрезается, громкость выравнивается,
частота — 22 050 Гц, как у голосов Piper medium.

Результат — out/piper/<out>/wavs/*.wav и metadata.csv («файл|текст»),
отчёт по отброшенным — rejected.csv там же.
Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\piper\\prepare.py
        [--raw raw,raw_b] [--drop-worst 0.15] [--out dataset_v2]
"""

from __future__ import annotations

import argparse
import csv
import sys
from pathlib import Path

import numpy as np
import pyloudnorm
import soundfile as sf
import torch
import torchaudio.functional as AF

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from quality import Quality  # noqa: E402
from select_best import similarity, words  # noqa: E402
from verify import OUT, RATE, recognizer, transcribe  # noqa: E402

PIPER = OUT / "piper"
TARGET_RATE = 22_050
TARGET_LUFS = -20.0
EDGE_DB = -40.0  # тише этого (от пика) по краям — тишина
MARGIN_SECS = 0.08


def trim(audio: np.ndarray, rate: int) -> np.ndarray:
    frame = int(0.01 * rate)
    n = len(audio) // frame
    if n == 0:
        return audio
    rms = np.sqrt((audio[: n * frame].reshape(n, frame) ** 2).mean(axis=1)) + 1e-9
    db = 20 * np.log10(rms / rms.max())
    loud = np.nonzero(db > EDGE_DB)[0]
    if len(loud) == 0:
        return audio
    margin = int(MARGIN_SECS * rate)
    start = max(0, loud[0] * frame - margin)
    end = min(len(audio), (loud[-1] + 1) * frame + margin)
    return audio[start:end]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--min-score", type=float, default=0.8)
    parser.add_argument("--raw", default="raw", help="папки дублей через запятую (raw,raw_b) — берётся лучший")
    parser.add_argument("--drop-worst", type=float, default=0.0, help="доля самых хриплых (по SIG) — убрать")
    parser.add_argument("--out", default="dataset", help="папка набора в out/piper")
    args = parser.parse_args()

    sentences = (PIPER / "sentences.txt").read_text(encoding="utf-8").splitlines()
    dataset = PIPER / args.out
    wavs = dataset / "wavs"
    wavs.mkdir(parents=True, exist_ok=True)
    rec = recognizer()
    quality = Quality()
    meter = pyloudnorm.Meter(TARGET_RATE)
    takes = [d.strip() for d in args.raw.split(",") if d.strip()]
    chosen, rejected = [], []
    for i, text in enumerate(sentences):
        best = None
        for take in takes:
            raw = PIPER / take / f"{i:04d}.wav"
            if not raw.exists():
                continue
            audio, rate = sf.read(raw, dtype="float32")
            mono = torch.from_numpy(trim(audio, rate))
            audio16 = AF.resample(mono, rate, RATE).numpy()
            heard = transcribe(rec, audio16)
            score = similarity(words(text), heard)
            if score < args.min_score:
                rejected.append((f"{take}/{raw.name}", f"{score:.2f}", "", text, heard))
                continue
            # Чистота голоса (DNSMOS SIG): хрип и искажения её снижают.
            sig = quality.mos(audio16)[0]
            if best is None or sig > best[0]:
                best = (sig, score, take, mono, rate)
        if best is not None:
            chosen.append((i, text, *best))

    # Самые хриплые из оставшихся — тоже вон.
    if chosen and args.drop_worst > 0:
        cutoff = float(np.quantile([c[2] for c in chosen], args.drop_worst))
        for c in chosen:
            if c[2] < cutoff:
                rejected.append((f"{c[4]}/{c[0]:04d}.wav", f"{c[3]:.2f}", f"{c[2]:.2f}", c[1], "хрип"))
        chosen = [c for c in chosen if c[2] >= cutoff]

    kept = []
    total = 0.0
    per_take: dict[str, int] = {}
    for i, text, _sig, _score, take, mono, rate in chosen:
        out = AF.resample(mono, rate, TARGET_RATE).numpy()
        gain = 10 ** ((TARGET_LUFS - meter.integrated_loudness(out)) / 20)
        out = out * gain
        peak = float(np.abs(out).max())
        if peak > 0.89:
            out = out * (0.89 / peak)
        sf.write(wavs / f"{i:04d}.wav", out, TARGET_RATE, subtype="PCM_16")
        kept.append((f"{i:04d}.wav", text))
        total += len(out) / TARGET_RATE
        per_take[take] = per_take.get(take, 0) + 1

    with open(dataset / "metadata.csv", "w", encoding="utf-8", newline="\n") as f:
        for name, text in kept:
            f.write(f"{name}|{text}\n")
    with open(dataset / "rejected.csv", "w", encoding="utf-8-sig", newline="") as f:
        w = csv.writer(f, delimiter=";")
        w.writerow(["file", "score", "sig", "text", "heard"])
        w.writerows(rejected)
    sigs = [c[2] for c in chosen]
    print(
        f"в наборе {len(kept)} фраз ({total / 60:.1f} мин речи), отброшено дублей {len(rejected)}; "
        f"взято из {per_take}; чистота SIG: средняя {np.mean(sigs):.2f}, худшая {min(sigs):.2f}"
    )


if __name__ == "__main__":
    main()
