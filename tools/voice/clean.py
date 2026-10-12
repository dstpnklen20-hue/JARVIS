"""Очистка реплик голоса от посторонних звуков (писки, гул, музыка).

Считает стемы для каждой реплики (tools/voice/out/stems/):
- vocals   — отделение голоса BS-RoFormer (модель для вокала);
- dry      — vocals + шумоподавление Mel-Band RoFormer denoise;
- dry_orig — шумоподавление прямо по исходнику (отделение голоса иногда
             съедает слова обработанного «электронного» голоса Джарвиса).
Лучший вариант каждой реплики выбирает select_best.py, он же нарезает итог
(`finalize`: моно, 22 050 Гц, пик -1 dBFS, тишина по краям обрезана).

Исходники не меняются. Повторный запуск считает только новые файлы.

Запуск: tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\clean.py [папка]
"""

from __future__ import annotations

import shutil
import sys
from pathlib import Path

import numpy as np
import soundfile as sf
import torch
import torchaudio.functional as AF
from audio_separator.separator import Separator

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / "data" / "jarvis-voice"
OUT = ROOT / "tools" / "voice" / "out"
MODELS = ROOT / "tools" / "voice" / "models"

VOCALS_MODEL = "model_bs_roformer_ep_317_sdr_12.9755.ckpt"
DENOISE_MODEL = "denoise_mel_band_roformer_aufr33_sdr_27.9959.ckpt"
TARGET_RATE = 22_050
# Не речь: песня целиком (музыка поверх).
SKIP = ("Песня целиком",)


def separate(sep: Separator, src: Path, stem: str, out_dir: Path, name: str) -> Path:
    """Прогоняет файл через загруженную модель и возвращает нужный стем."""
    out_dir.mkdir(parents=True, exist_ok=True)
    sep.output_dir = str(out_dir)
    if sep.model_instance is not None:
        sep.model_instance.output_dir = str(out_dir)
    produced = sep.separate(str(src))
    for p in produced:
        path = Path(p) if Path(p).is_absolute() else out_dir / p
        if f"({stem})".lower() in path.name.lower():
            target = out_dir / f"{name}.wav"
            shutil.move(path, target)
            for other in produced:
                o = Path(other) if Path(other).is_absolute() else out_dir / other
                if o.exists() and o != target:
                    o.unlink()
            return target
    raise RuntimeError(f"{src.name}: нет стема {stem} среди {produced}")


def finalize(src: Path, dst: Path) -> float:
    """Моно, 22 050 Гц, обрезка тишины, пик -1 dBFS. Возвращает длительность."""
    audio, rate = sf.read(src, always_2d=True, dtype="float32")
    mono = torch.from_numpy(audio.mean(axis=1))
    mono = AF.resample(mono, rate, TARGET_RATE).numpy()
    # Тишина по краям: громкость кадров 10 мс ниже пика на 40 дБ, запас 150 мс.
    # Порог от пика, а не абсолютный: на тихих записях иначе срезалось бы «сэр».
    frame = int(0.01 * TARGET_RATE)
    n = len(mono) // frame
    if n:
        rms = np.sqrt((mono[: n * frame].reshape(n, frame) ** 2).mean(axis=1))
        loud = np.flatnonzero(rms > rms.max() * 10 ** (-40 / 20))
        pad = int(0.15 * TARGET_RATE)
        mono = mono[max(0, loud[0] * frame - pad) : (loud[-1] + 1) * frame + pad]
    peak = float(np.abs(mono).max()) or 1.0
    mono = mono * (10 ** (-1 / 20) / peak)
    dst.parent.mkdir(parents=True, exist_ok=True)
    sf.write(dst, mono, TARGET_RATE, subtype="PCM_16")
    return len(mono) / TARGET_RATE


def main() -> None:
    src_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else SRC
    files = sorted(
        p for p in src_dir.rglob("*.wav") if not any(s in p.stem for s in SKIP)
    )
    print(f"Файлов: {len(files)}; CUDA: {torch.cuda.is_available()}")
    MODELS.mkdir(parents=True, exist_ok=True)
    sep = Separator(
        model_file_dir=str(MODELS),
        output_dir=str(OUT),
        output_format="WAV",
        log_level=30,  # WARNING
    )

    # Имена — по пути внутри архива (в подпапках бывают одинаковые имена).
    names = {f: "__".join(f.relative_to(src_dir).with_suffix("").parts) for f in files}

    # Готовые стемы не пересчитываются (повторный запуск — только новое).
    def cached(stage: str, f: Path, make) -> Path:
        path = OUT / "stems" / stage / f"{names[f]}.wav"
        return path if path.exists() else make()

    sep.load_model(model_filename=VOCALS_MODEL)
    vocals = {
        f: cached("vocals", f, lambda f=f: separate(sep, f, "Vocals", OUT / "stems" / "vocals", names[f]))
        for f in files
    }

    # Два варианта шумоподавления: после отделения голоса и сразу по исходнику —
    # отделение голоса иногда съедает обработанный «электронный» голос Джарвиса.
    sep.load_model(model_filename=DENOISE_MODEL)
    for f in files:
        cached("dry", f, lambda f=f: separate(sep, vocals[f], "dry", OUT / "stems" / "dry", names[f]))
        cached("dry_orig", f, lambda f=f: separate(sep, f, "dry", OUT / "stems" / "dry_orig", names[f]))
    print(f"Стемы готовы: {OUT / 'stems'}. Выбор лучшего варианта — select.py")


if __name__ == "__main__":
    main()
