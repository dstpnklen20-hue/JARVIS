"""Выбор лучшего варианта каждой реплики и итоговая нарезка.

Варианты — стемы из out/stems/ (отделение голоса, восстановление речи
MossFormer2 по исходнику и по стему голоса) и сам исходник. Для каждого:
разборчивость (распознавание против текста из имени файла), тембр
(похожесть на эталон «Запрос выполнен сэр»), DNSMOS (quality.py).

Правило: среди вариантов, где слова не потерялись (разборчивость не ниже
лучшей − 0.05), а из них — где тембр не уехал (не ниже лучшего среди них
− 0.10), берётся вариант
с самой высокой общей оценкой DNSMOS (OVRL). Громкость всех итоговых реплик
выравнивается под эталон (LUFS), чтобы они звучали ровно.

Уровни (по OVRL итогового варианта, эталон ≈ 3.4):
  «как эталон» ≥ 3.0, «средняя» ≥ 2.5, «слабая» < 2.5.

Итог — out/clean/, отчёт — out/report.csv.
Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\select_best.py
"""

from __future__ import annotations

import csv
import difflib
import re
import shutil
from pathlib import Path

import numpy as np
import pyloudnorm
import soundfile as sf
from num2words import num2words

from clean import TARGET_RATE, finalize
from quality import Quality
from verify import OUT, SRC, load, recognizer, transcribe

REFERENCE = "Запрос выполнен сэр"
# Порядок важен только при равных оценках: чище — раньше.
VARIANTS = [
    ("восстановление голоса", "se_vocals"),
    ("восстановление", "se_orig"),
    ("отделение голоса", "vocals"),
    ("исходник", None),
]
WORDS_TOLERANCE = 0.05
TIMBRE_TOLERANCE = 0.10
SKIP = ("Песня целиком",)


def tier(ovrl: float) -> str:
    return "как эталон" if ovrl >= 3.0 else "средняя" if ovrl >= 2.5 else "слабая"


def reference(name: str) -> str:
    """Ожидаемый текст из имени файла: числа словами, без пометок."""
    text = re.sub(r"\((второй|второе)\)|=\)|\s2$", " ", name)
    text = re.sub(r"\d+", lambda m: " " + num2words(int(m.group()), lang="ru") + " ", text)
    return words(text)


def words(text: str) -> str:
    text = text.lower().replace("ё", "е")
    # «процент», «процента», «процентов» и знак % — одно и то же.
    return " ".join(w for w in re.findall(r"[а-я]+", text) if not w.startswith("процент"))


def similarity(expected: str, heard: str) -> float:
    return difflib.SequenceMatcher(None, expected, words(heard)).ratio()


def match_loudness(path: Path, target_lufs: float) -> None:
    audio, rate = sf.read(path, dtype="float32")
    meter = pyloudnorm.Meter(rate, block_size=0.2)
    gain = 10 ** ((target_lufs - meter.integrated_loudness(audio)) / 20)
    audio = audio * gain
    peak = float(np.abs(audio).max())
    if peak > 0.89:  # не выше -1 dBFS
        audio = audio * (0.89 / peak)
    sf.write(path, audio, rate, subtype="PCM_16")


def main() -> None:
    q = Quality()
    rec = recognizer()
    files = sorted(p for p in SRC.rglob("*.wav") if not any(s in p.stem for s in SKIP))
    names = {f: "__".join(f.relative_to(SRC).with_suffix("").parts) for f in files}
    ref_file = next(f for f in files if f.stem.strip() == REFERENCE)
    ref_emb = q.embed(load(OUT / "stems" / "vocals" / f"{names[ref_file]}.wav"))

    clean = OUT / "clean"
    clean.mkdir(parents=True, exist_ok=True)
    for old in clean.glob("*.wav"):
        old.unlink()

    rows = []
    for f in files:
        expected = reference(f.stem)
        scored = []
        for label, stage in VARIANTS:
            path = f if stage is None else OUT / "stems" / stage / f"{names[f]}.wav"
            if not path.exists():
                continue
            x = load(path)
            x = x / (np.abs(x).max() or 1.0)
            heard = transcribe(rec, x)
            sig, bak, ovr = q.mos(x)
            scored.append(
                {
                    "label": label,
                    "path": path,
                    "heard": heard,
                    "words": similarity(expected, heard),
                    "timbre": float(q.embed(x) @ ref_emb),
                    "sig": sig,
                    "bak": bak,
                    "ovr": ovr,
                }
            )
        best_words = max(s["words"] for s in scored)
        intact = [s for s in scored if s["words"] >= best_words - WORDS_TOLERANCE]
        best_timbre = max(s["timbre"] for s in intact)
        allowed = [s for s in intact if s["timbre"] >= best_timbre - TIMBRE_TOLERANCE]
        chosen = max(allowed, key=lambda s: s["ovr"])
        secs = finalize(chosen["path"], clean / f"{names[f]}.wav")
        rows.append(
            {
                "file": f.stem.strip(),
                "tier": tier(chosen["ovr"]),
                "variant": chosen["label"],
                "OVRL": round(chosen["ovr"], 2),
                "SIG": round(chosen["sig"], 2),
                "BAK": round(chosen["bak"], 2),
                "timbre": round(chosen["timbre"], 2),
                "words": round(chosen["words"], 2),
                "OVRL_source": round(next(s["ovr"] for s in scored if s["label"] == "исходник"), 2),
                "heard": chosen["heard"],
                "secs": round(secs, 1),
                "name": names[f],
            }
        )

    # Громкость — как у эталона.
    meter = pyloudnorm.Meter(TARGET_RATE, block_size=0.2)
    ref_audio, _ = sf.read(clean / f"{names[ref_file]}.wav", dtype="float32")
    target = meter.integrated_loudness(ref_audio)
    for r in rows:
        match_loudness(clean / f"{r['name']}.wav", target)

    # Копии по уровням — удобно слушать.
    tiers = OUT / "по уровням"
    if tiers.exists():
        shutil.rmtree(tiers)
    for r in rows:
        (tiers / r["tier"]).mkdir(parents=True, exist_ok=True)
        shutil.copyfile(clean / f"{r['name']}.wav", tiers / r["tier"] / f"{r['OVRL']:.2f} {r['file']}.wav")

    rows.sort(key=lambda r: -r["OVRL"])
    with open(OUT / "report.csv", "w", newline="", encoding="utf-8-sig") as out:
        writer = csv.DictWriter(out, fieldnames=list(rows[0]), delimiter=";")
        writer.writeheader()
        writer.writerows(rows)
    for r in rows:
        print(
            f"{r['OVRL']:.2f} (было {r['OVRL_source']:.2f}) {r['tier']:<10} {r['variant']:<22} "
            f"т{r['timbre']:.2f} р{r['words']:.2f}  {r['file'][:48]}"
        )
    for t in ("как эталон", "средняя", "слабая"):
        group = [r for r in rows if r["tier"] == t]
        print(f"{t}: {len(group)} реплик, {sum(r['secs'] for r in group) / 60:.1f} мин")
    print(f"Громкость выровнена под эталон: {target:.1f} LUFS")


if __name__ == "__main__":
    main()
