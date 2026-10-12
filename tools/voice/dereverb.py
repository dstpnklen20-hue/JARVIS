"""Удаление реверберации (эхо помещения и эффекты фильма) — MelBand RoFormer
De-Reverb (anvuew, SDR 19.2). Реверберация — частая причина «дребезжания»
и «роботизированности», и клон голоса её копирует.

Стемы: out/stems/dv_vocals (по стему голоса), out/stems/dv_se (по
восстановленному MossFormer2). Запускать после clean.py и enhance.py:
  tools\\voice\\.venv\\Scripts\\python.exe tools\\voice\\dereverb.py
Дальше select_best.py учитывает и эти варианты.
"""

from __future__ import annotations

from audio_separator.separator import Separator

from clean import MODELS, OUT, SKIP, SRC, separate

DEREVERB_MODEL = "dereverb_mel_band_roformer_anvuew_sdr_19.1729.ckpt"
STAGES = {"dv_vocals": "vocals", "dv_se": "se_orig"}


def main() -> None:
    files = sorted(p for p in SRC.rglob("*.wav") if not any(s in p.stem for s in SKIP))
    names = ["__".join(f.relative_to(SRC).with_suffix("").parts) for f in files]
    sep = Separator(model_file_dir=str(MODELS), output_dir=str(OUT), output_format="WAV", log_level=30)
    sep.load_model(model_filename=DEREVERB_MODEL)
    done = 0
    for stage, source in STAGES.items():
        for name in names:
            src = OUT / "stems" / source / f"{name}.wav"
            dst = OUT / "stems" / stage / f"{name}.wav"
            if dst.exists() or not src.exists():
                continue
            separate(sep, src, "noreverb", dst.parent, name)
            done += 1
    print(f"Без реверберации: {done} новых файлов -> {OUT / 'stems'}")


if __name__ == "__main__":
    main()
