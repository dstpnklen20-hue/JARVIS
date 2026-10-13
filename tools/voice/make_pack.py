"""Звуковой пакет sounds/jarvis/ по вашим оценкам на слух (data/оценки_раунд2.txt).

Берутся все варианты с оценкой «эталон» — Джарвис чередует их. Строки вида
`ok_1:итог/ref_dry_3 = эталон`: id фразы, откуда дубль, оценка. «!» в конце
(`= эталон !`) — паузу перед «сэр» сократить вдвое (tighten.py).
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
from tighten import tighten

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "tools" / "voice" / "out"
PACK = ROOT / "sounds" / "jarvis"
# Раунд 3 — новые фразы (not_heard, out_of_range, команды); файла может не быть.
RATINGS = [ROOT / "data" / "оценки_раунд2.txt", ROOT / "data" / "оценки_раунд3.txt"]
REPLIES = ("wake", "ok", "not_understood", "failed", "ask", "denied", "not_heard", "out_of_range", "thinking", "offline")
# Фраза, которая по смыслу относится к другому ответу, чем говорит её id:
# «Простите, сэр, я не расслышал» — ответ на тишину после «Джарвис».
GROUP_OF = {"not_understood_1": "not_heard"}
# Фразы для личных команд (commands/my, не в git): id → имя файла там.
# Берётся первый вариант с оценкой «эталон».
COMMAND_SOUNDS = {"game_ask_1": "game_ask_1.wav", "game_loading_1": "game_loading_1.wav", "game_enjoy_1": "game_enjoy_1.wav"}
COMMAND_PACK = ROOT / "commands" / "my"
# Дубли, где пауза перед «сэр» вдвое короче (отмечены «!»): (id, источник, дубль).
TIGHTEN: set[tuple[str, str, str]] = set()
SOURCES = {"итог": None, "f5v2": OUT / "f5v2", "раунд1": OUT / "f5"}
# «Да, сэр»: лучший из дублей, где распознаются оба слова (out/f5_yes).
YES_TAKE = OUT / "f5_yes" / "wake_1_0.6_1" / "ref_short_6.wav"
# Эталон из фильма — тоже в «выполнено».
ETALON = "Запрос выполнен сэр"
# Вопрос-разрешение — одна общая фраза, подробности в окне (ваше решение).
# Прежние варианты ask_* — в запасе.
ASK = [OUT / "f5_ask_best" / "ask_generic.wav"]
ASK_SPARE = {"ask_generic_sir": OUT / "f5_ask_best" / "ask_generic_sir.wav"}
# Приветствия при запуске: фраза → часть суток.
GREETINGS = {"greeting_1": "morning", "greeting_2": "day", "greeting_3": "evening", "greeting_4": "night"}


def rated() -> dict[str, list[tuple[str, str]]]:
    """{id: [(источник, дубль)]} с оценкой «эталон», без повторов. Отмеченные «!»
    попадают в TIGHTEN."""
    out: dict[str, list[tuple[str, str]]] = defaultdict(list)
    lines = [line for path in RATINGS if path.exists() for line in path.read_text(encoding="utf-8").splitlines()]
    for line in lines:
        m = re.match(r"(\w+):(итог|f5v2|раунд1)/(\S+)\s*=\s*эталон\s*(!)?", line.strip())
        if m and (m.group(2), m.group(3)) not in out[m.group(1)]:
            out[m.group(1)].append((m.group(2), m.group(3)))
        if m and m.group(4):
            TIGHTEN.add((m.group(1), m.group(2), m.group(3)))
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
    greetings: dict[str, list[Path]] = defaultdict(list)
    extra: list[Path] = []
    choices = rated()
    choices["wake_1"] = [("yes", "")]
    choices.setdefault("ok_0", []).append(("etalon", ""))
    for pid in sorted(choices):
        group = GROUP_OF.get(pid, pid.rsplit("_", 1)[0])
        for n, (source, take) in enumerate(choices[pid], 1):
            if source == "итог":
                src, ready = OUT / "f5v2_best" / f"{pid}.wav", True
            elif source == "yes":
                src, ready = YES_TAKE, False
            elif source == "etalon":
                src, ready = etalon_path, False
            else:
                src, ready = SOURCES[source] / pid / f"{take}.wav", False
            # Вопрос — общая фраза ASK, прежние варианты — в запас.
            in_pack = group in REPLIES and group != "ask"
            if in_pack:
                dst = PACK / f"{group}_{len(groups[group]) + 1}.wav"
            elif pid in GREETINGS:
                dst = PACK / f"{pid}_{n}.wav"
            else:
                dst = PACK / "extra" / f"{pid}_{n}.wav"
            if ready:
                shutil.copyfile(src, dst)
            else:
                finalize(src, dst)
                polish(dst, target)
            if (pid, source, take) in TIGHTEN:
                before, after = tighten(dst, 0.5)
                print(f"{dst.name}: пауза перед «сэр» {before:.2f} с → {after:.2f} с")
            if pid in COMMAND_SOUNDS and n == 1 and COMMAND_PACK.is_dir():
                shutil.copyfile(dst, COMMAND_PACK / COMMAND_SOUNDS[pid])
                print(f"{pid} → commands/my/{COMMAND_SOUNDS[pid]}")
            if in_pack:
                groups[group].append(dst)
            elif pid in GREETINGS:
                greetings[GREETINGS[pid]].append(dst)
            else:
                extra.append(dst)
    for i, src in enumerate(ASK, 1):
        dst = PACK / f"ask_{i}.wav"
        shutil.copyfile(src, dst)
        groups["ask"].append(dst)
    for name, src in ASK_SPARE.items():
        shutil.copyfile(src, PACK / "extra" / f"{name}.wav")
        extra.append(PACK / "extra" / f"{name}.wav")

    def files(paths: list[Path]) -> str:
        return ", ".join(f'"{p.name}"' for p in paths)

    lines = ['name = "Джарвис (голос по эталону, только для личного использования)"', "", "[replies]"]
    lines += [f"{reply} = [{files(groups[reply])}]" for reply in REPLIES if groups.get(reply)]
    lines += ["", "[greetings]"]
    lines += [f"{part} = [{files(greetings[part])}]" for part in ("morning", "day", "evening", "night") if greetings[part]]
    (PACK / "pack.toml").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print((PACK / "pack.toml").read_text(encoding="utf-8"))
    print("Запас для команд (extra):", ", ".join(sorted(p.name for p in extra)))


if __name__ == "__main__":
    main()
