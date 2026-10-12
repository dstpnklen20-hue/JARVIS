"""Звуковой пакет sounds/jarvis/ по вашим оценкам на слух (data/оценки_раунд2.txt).

Берутся все варианты с оценкой «эталон» — Джарвис чередует их. Строки вида
`ok_1:итог/ref_dry_3 = эталон`: id фразы, откуда дубль, оценка.
- итог/<дубль>   — out/f5v2_best/<id>.wav (уже обработан);
- f5v2/<дубль>   — сырой дубль раунда 2, обрабатывается здесь;
- раунд1/<дубль> — сырой дубль раунда 1 (out/f5), обрабатывается здесь.
Ответы пакета — по началу id (wake, ok, …); остальное (greeting, goodbye,
status) — в sounds/jarvis/extra/ для будущих команд `say`.
«Да, сэр» — отдельный дубль (YES_TAKE): в раунде 2 синтез терял «сэр».

Пакет только для личного использования — в git не попадает (.gitignore).
Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\make_pack.py
"""

from __future__ import annotations

import csv
import re
import shutil
from collections import defaultdict
from pathlib import Path

import pyloudnorm
import soundfile as sf

from clean import TARGET_RATE, finalize
from polish import polish

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "tools" / "voice" / "out"
PACK = ROOT / "sounds" / "jarvis"
RATINGS = ROOT / "data" / "оценки_раунд2.txt"
REPLIES = ("wake", "ok", "not_understood", "failed", "ask", "denied")
SOURCES = {"итог": None, "f5v2": OUT / "f5v2", "раунд1": OUT / "f5"}
# «Да, сэр»: лучший из дублей, где распознаются оба слова (out/f5_yes).
YES_TAKE = OUT / "f5_yes" / "wake_1_0.6_1" / "ref_short_6.wav"
# Эталон из фильма — тоже в «выполнено».
ETALON = "Запрос выполнен сэр"


def rated() -> dict[str, list[tuple[str, str]]]:
    """{id: [(источник, дубль)]} с оценкой «эталон», без повторов."""
    out: dict[str, list[tuple[str, str]]] = defaultdict(list)
    for line in RATINGS.read_text(encoding="utf-8").splitlines():
        m = re.match(r"(\w+):(итог|f5v2|раунд1)/(\S+)\s*=\s*эталон", line.strip())
        if m and (m.group(2), m.group(3)) not in out[m.group(1)]:
            out[m.group(1)].append((m.group(2), m.group(3)))
    return out


def main() -> None:
    meter = pyloudnorm.Meter(TARGET_RATE, block_size=0.2)
    with open(OUT / "report.csv", encoding="utf-8-sig") as f:
        etalon = next(r["name"] for r in csv.DictReader(f, delimiter=";") if r["file"] == ETALON)
    etalon_path = OUT / "clean" / f"{etalon}.wav"
    target = meter.integrated_loudness(sf.read(etalon_path, dtype="float32")[0])

    if PACK.exists():
        shutil.rmtree(PACK)
    (PACK / "extra").mkdir(parents=True)

    groups: dict[str, list[Path]] = defaultdict(list)
    choices = rated()
    choices["wake_1"] = [("yes", "")]
    choices.setdefault("ok_0", []).append(("etalon", ""))
    for pid in sorted(choices):
        group = pid.rsplit("_", 1)[0]
        for source, take in choices[pid]:
            if source == "итог":
                src, ready = OUT / "f5v2_best" / f"{pid}.wav", True
            elif source == "yes":
                src, ready = YES_TAKE, False
            elif source == "etalon":
                src, ready = etalon_path, False
            else:
                src, ready = SOURCES[source] / pid / f"{take}.wav", False
            index = len(groups[group]) + 1
            dst = (PACK if group in REPLIES else PACK / "extra") / f"{group}_{index}.wav"
            if ready:
                shutil.copyfile(src, dst)
            else:
                finalize(src, dst)
                polish(dst, target)
            groups[group].append(dst)

    lines = ['name = "Джарвис (голос по эталону, только для личного использования)"', "", "[replies]"]
    for reply in REPLIES:
        files = ", ".join(f'"{p.name}"' for p in groups.get(reply, []))
        lines.append(f"{reply} = [{files}]")
    (PACK / "pack.toml").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print((PACK / "pack.toml").read_text(encoding="utf-8"))
    extra = sorted(p.name for g, ps in groups.items() if g not in REPLIES for p in ps)
    print("Запас для команд (extra):", ", ".join(extra))


if __name__ == "__main__":
    main()
