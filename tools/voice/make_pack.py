"""Звуковой пакет sounds/jarvis/ из очищенных реплик (tools/voice/out/clean).

Пакет только для личного использования — в git не попадает (.gitignore).
Чего в репликах нет («не удалось», «разрешите?»), берётся из стандартного
пакета, пока не будет голоса синтеза Джарвиса.
Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\make_pack.py
"""

from __future__ import annotations

import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CLEAN = ROOT / "tools" / "voice" / "out" / "clean"
PACK = ROOT / "sounds" / "jarvis"
PREFIX = "Jarvis Sound Pack от Jarvis Desktop__"

REPLIES: dict[str, list[str]] = {
    "wake": ["Да сэр", "Да сэр(второй)", "К вашим услугам сэр", "Всегда к вашим услугам сэр"],
    "ok": ["Запрос выполнен сэр", "Есть"],
    "not_understood": ["Чего вы пытаетесь добиться сэр"],
    "denied": ["Как пожелаете "],
    # Нет подходящих реплик — стандартный пакет.
    "failed": ["../default/failed_1.wav"],
    "ask": ["../default/ask_1.wav", "../default/ask_2.wav"],
}


def main() -> None:
    PACK.mkdir(parents=True, exist_ok=True)
    lines = ['name = "Джарвис (фильм, только для личного использования)"', "", "[replies]"]
    for reply, sources in REPLIES.items():
        files = []
        for i, src in enumerate(sources, 1):
            if src.startswith("../"):
                files.append(src)
                continue
            target = f"{reply}_{i}.wav"
            shutil.copyfile(CLEAN / f"{PREFIX}{src}.wav", PACK / target)
            files.append(target)
        lines.append(f"{reply} = [{', '.join(repr(f).replace(chr(39), chr(34)) for f in files)}]")
    (PACK / "pack.toml").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print((PACK / "pack.toml").read_text(encoding="utf-8"))


if __name__ == "__main__":
    main()
