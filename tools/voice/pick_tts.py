"""Выбор лучшего дубля синтезированных реплик (out/tts/<id>/*.wav).

Для каждого дубля: тембр (CAM++, похожесть на эталон «Запрос выполнен сэр»),
DNSMOS (quality.py), разборчивость (распознавание против текста).
Правило: среди дублей без потерянных слов (не ниже лучшего − 0.05 и не ниже
0.8) берётся дубль с наибольшим «тембр + 0.15 × OVRL»: главное — похожесть
на эталон, качество — при прочих равных. Для сравнения печатается, насколько
похожи на эталон настоящие реплики Джарвиса из фильма.

Источник — out/tts (Chatterbox, clone.py) или out/f5 (ESpeech, clone_f5.py).
Итог — out/<источник>_best/<id>.wav (громкость как у эталона), отчёт —
out/<источник>_report.csv.
Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\pick_tts.py [tts | f5]
"""

from __future__ import annotations

import csv
import sys
from collections import defaultdict

import numpy as np
import pyloudnorm
import soundfile as sf

from clean import TARGET_RATE, finalize
from quality import Quality
from select_best import REFERENCE, match_loudness, similarity, words
from verify import OUT, load, recognizer, transcribe

TTS = OUT / (sys.argv[1] if len(sys.argv) > 1 else "tts")
BEST = OUT / f"{TTS.name}_best"
MIN_WORDS = 0.8


def main() -> None:
    q = Quality()
    rec = recognizer()
    with open(OUT / "report.csv", encoding="utf-8-sig") as f:
        real = list(csv.DictReader(f, delimiter=";"))
    ref_row = next(r for r in real if r["file"] == REFERENCE)
    ref_path = OUT / "clean" / f"{ref_row['name']}.wav"
    ref_emb = q.embed(load(ref_path))

    BEST.mkdir(parents=True, exist_ok=True)
    rows, by_ref = [], defaultdict(list)
    for phrase_dir in sorted(p for p in TTS.iterdir() if p.is_dir()):
        text = (phrase_dir / "text.txt").read_text(encoding="utf-8")
        expected = words(text)
        takes = []
        for wav in sorted(phrase_dir.glob("*.wav")):
            x = load(wav)
            x = x / (np.abs(x).max() or 1.0)
            heard = transcribe(rec, x)
            sig, bak, ovr = q.mos(x)
            take = {
                "path": wav,
                "ref": wav.stem.rsplit("_", 1)[0],
                "heard": heard,
                "words": similarity(expected, heard),
                "timbre": float(q.embed(x) @ ref_emb),
                "sig": sig,
                "ovr": ovr,
            }
            takes.append(take)
            by_ref[take["ref"]].append(take)
        best_words = max(t["words"] for t in takes)
        intact = [t for t in takes if t["words"] >= max(best_words - 0.05, MIN_WORDS)] or takes
        chosen = max(intact, key=lambda t: t["timbre"] + 0.15 * t["ovr"])
        finalize(chosen["path"], BEST / f"{phrase_dir.name}.wav")
        rows.append(
            {
                "id": phrase_dir.name,
                "text": text,
                "take": chosen["path"].stem,
                "timbre": round(chosen["timbre"], 2),
                "OVRL": round(chosen["ovr"], 2),
                "SIG": round(chosen["sig"], 2),
                "words": round(chosen["words"], 2),
                "heard": chosen["heard"],
            }
        )
        print(
            f"{phrase_dir.name:<17} т{chosen['timbre']:.2f} {chosen['ovr']:.2f} р{chosen['words']:.2f} "
            f"{chosen['path'].stem:<12} «{text}» → «{chosen['heard']}»"
        )

    meter = pyloudnorm.Meter(TARGET_RATE, block_size=0.2)
    target = meter.integrated_loudness(sf.read(ref_path, dtype="float32")[0])
    for r in rows:
        match_loudness(BEST / f"{r['id']}.wav", target)
    with open(OUT / f"{TTS.name}_report.csv", "w", newline="", encoding="utf-8-sig") as f:
        writer = csv.DictWriter(f, fieldnames=list(rows[0]), delimiter=";")
        writer.writeheader()
        writer.writerows(rows)

    print("\nОбразцы (все дубли): тембр / OVRL / разборчивость")
    for ref, takes in sorted(by_ref.items()):
        a = np.mean([(t["timbre"], t["ovr"], t["words"]) for t in takes], axis=0)
        print(f"  {ref:<10} {a[0]:.2f} / {a[1]:.2f} / {a[2]:.2f}")
    top = [r for r in real if r["tier"] == "как эталон" and r["file"] != REFERENCE]
    print(
        f"Для сравнения, настоящие реплики «как эталон»: тембр {np.mean([float(r['timbre']) for r in top]):.2f}, "
        f"OVRL {np.mean([float(r['OVRL']) for r in top]):.2f}"
    )
    chosen = np.mean([(r["timbre"], r["OVRL"]) for r in rows], axis=0)
    print(f"Выбранные дубли: тембр {chosen[0]:.2f}, OVRL {chosen[1]:.2f}")


if __name__ == "__main__":
    main()
