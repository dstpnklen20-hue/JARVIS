"""Проверка очистки: распознаёт реплики до и после, сравнивает с текстом
из имени файла и меряет фон (уровень самых тихих участков).

Пишет tools/voice/out/report.csv и печатает сводку.
Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\verify.py
"""

from __future__ import annotations

import csv
import difflib
import re
from pathlib import Path

import numpy as np
import sherpa_onnx
import soundfile as sf
import torch
import torchaudio.functional as AF

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / "data" / "jarvis-voice"
OUT = ROOT / "tools" / "voice" / "out"
ASR = ROOT / "models" / "sherpa-onnx-streaming-zipformer-small-ru-vosk-int8-2025-08-16"
RATE = 16_000


def recognizer() -> sherpa_onnx.OnlineRecognizer:
    return sherpa_onnx.OnlineRecognizer.from_transducer(
        tokens=str(ASR / "tokens.txt"),
        encoder=str(ASR / "encoder.int8.onnx"),
        decoder=str(ASR / "decoder.onnx"),
        joiner=str(ASR / "joiner.int8.onnx"),
        num_threads=4,
        sample_rate=RATE,
        decoding_method="greedy_search",
    )


def load(path: Path) -> np.ndarray:
    audio, rate = sf.read(path, always_2d=True, dtype="float32")
    mono = torch.from_numpy(audio.mean(axis=1))
    return AF.resample(mono, rate, RATE).numpy()


def transcribe(rec: sherpa_onnx.OnlineRecognizer, samples: np.ndarray) -> str:
    stream = rec.create_stream()
    stream.accept_waveform(RATE, samples)
    stream.accept_waveform(RATE, np.zeros(int(0.8 * RATE), dtype=np.float32))
    stream.input_finished()
    while rec.is_ready(stream):
        rec.decode_stream(stream)
    return rec.get_result(stream).strip().lower()


def normalize(text: str) -> str:
    text = text.lower().replace("ё", "е")
    text = re.sub(r"\((второй|второе)\)|\b2\b|=\)", " ", text)
    return " ".join(re.findall(r"[а-я]+", text))


def similarity(a: str, b: str) -> float:
    return difflib.SequenceMatcher(None, normalize(a), normalize(b)).ratio()


def floor_db(samples: np.ndarray) -> float:
    """Уровень фона: 10-й перцентиль громкости кадров по 20 мс."""
    frame = int(0.02 * RATE)
    n = len(samples) // frame
    if n == 0:
        return -120.0
    rms = np.sqrt((samples[: n * frame].reshape(n, frame) ** 2).mean(axis=1) + 1e-12)
    return float(20 * np.log10(np.percentile(rms, 10) + 1e-9))


def peak_norm(samples: np.ndarray) -> np.ndarray:
    peak = float(np.abs(samples).max()) or 1.0
    return samples / peak


def main() -> None:
    rec = recognizer()
    rows = []
    for clean in sorted((OUT / "clean").glob("*.wav")):
        rel = Path(*clean.stem.split("__")).with_suffix(".wav")
        orig = SRC / rel
        if not orig.exists():
            matches = list(SRC.rglob(rel.name))
            orig = matches[0] if matches else None
        expected = rel.stem
        before = peak_norm(load(orig)) if orig else None
        after = peak_norm(load(clean))
        heard_before = transcribe(rec, before) if before is not None else ""
        heard_after = transcribe(rec, after)
        rows.append(
            {
                "file": clean.stem,
                "expected": expected,
                "heard_before": heard_before,
                "heard_after": heard_after,
                "match_before": round(similarity(expected, heard_before), 2),
                "match_after": round(similarity(expected, heard_after), 2),
                "floor_before_db": round(floor_db(before), 1) if before is not None else "",
                "floor_after_db": round(floor_db(after), 1),
                "secs": round(len(after) / RATE, 1),
            }
        )
    with open(OUT / "report.csv", "w", newline="", encoding="utf-8-sig") as f:
        writer = csv.DictWriter(f, fieldnames=list(rows[0]), delimiter=";")
        writer.writeheader()
        writer.writerows(rows)

    for r in sorted(rows, key=lambda r: r["match_after"]):
        print(
            f"{r['match_before']:.2f} -> {r['match_after']:.2f} | фон {r['floor_before_db']:>6} -> "
            f"{r['floor_after_db']:>6} дБ | {r['secs']:4.1f} с | {r['expected'][:45]:<45} | «{r['heard_after']}»"
        )
    mb = np.mean([r["match_before"] for r in rows])
    ma = np.mean([r["match_after"] for r in rows])
    fb = np.median([r["floor_before_db"] for r in rows if r["floor_before_db"] != ""])
    fa = np.median([r["floor_after_db"] for r in rows])
    good = [r for r in rows if r["match_after"] >= 0.7]
    print(
        f"\nСовпадение с текстом: {mb:.2f} -> {ma:.2f}; фон (медиана): {fb:.0f} -> {fa:.0f} дБ; "
        f"разборчивых (>= 0.7): {len(good)} из {len(rows)}, {sum(r['secs'] for r in good) / 60:.1f} мин"
    )


if __name__ == "__main__":
    main()
