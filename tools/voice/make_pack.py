"""Звуковой пакет sounds/jarvis/ из очищенных реплик (tools/voice/out/clean).

Пакет только для личного использования — в git не попадает (.gitignore).
Чего в репликах нет («не удалось», «разрешите?»), синтезировано голосом
эталона (out/tts_best/).
Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\make_pack.py
"""

from __future__ import annotations

import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CLEAN = ROOT / "tools" / "voice" / "out" / "clean"
PACK = ROOT / "sounds" / "jarvis"
PREFIX = "Jarvis Sound Pack от Jarvis Desktop__"

# Настоящие реплики, близкие к эталону «Запрос выполнен сэр» (out/report.csv),
# и синтез голосом эталона «tts:<id>» (clone.py → pick_tts.py, out/tts_best/).
# Синтез закрывает пробелы и заменяет реплики, испорченные в самом фильме
# («К вашим услугам», «Всегда к вашим услугам», «Как пожелаете»: DNSMOS ≤ 2.4).
REPLIES: dict[str, list[str]] = {
    "wake": ["Да сэр(второй)", "tts:wake_2", "tts:wake_3", "tts:wake_4"],
    "ok": ["Запрос выполнен сэр", "tts:ok_1"],
    "not_understood": ["Чего вы пытаетесь добиться сэр", "tts:not_understood_1", "tts:not_understood_2"],
    "failed": ["tts:failed_1", "tts:failed_2"],
    "ask": ["tts:ask_1", "tts:ask_2"],
    "denied": ["tts:denied_1", "tts:denied_2"],
}
TTS_BEST = ROOT / "tools" / "voice" / "out" / "tts_best"


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
            source = TTS_BEST / f"{src[4:]}.wav" if src.startswith("tts:") else CLEAN / f"{PREFIX}{src}.wav"
            shutil.copyfile(source, PACK / target)
            files.append(target)
        lines.append(f"{reply} = [{', '.join(repr(f).replace(chr(39), chr(34)) for f in files)}]")
    (PACK / "pack.toml").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print((PACK / "pack.toml").read_text(encoding="utf-8"))


if __name__ == "__main__":
    main()
