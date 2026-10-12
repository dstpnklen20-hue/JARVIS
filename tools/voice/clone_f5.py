"""Реплики голосом эталона — ESpeech-TTS (F5-TTS, обучена на русском,
Apache-2.0). В отличие от Chatterbox ударения задаются явно: «+» перед
ударной гласной. Расставляет их RUAccent, поверх — словарь stress.txt.

Образцы голоса:
- ref_short — эталон «Запрос выполнен сэр»;
- ref_long  — эталон + реплики «как эталон» с близким тембром (out/tts/ref_long.wav
  из clone.py, тексты — out/tts/references.json).

Запуск (отдельное окружение):
  tools\\voice\\.venv-f5\\Scripts\\python.exe tools\\voice\\clone_f5.py [текст --id имя] [--takes N]
Дубли — out/f5/<id>/<образец>_<n>.wav; лучший выбирает pick_tts.py out/f5.
"""

from __future__ import annotations

import argparse
import json
import re
import time
from pathlib import Path

import soundfile as sf
import torch
from f5_tts.infer.utils_infer import infer_process, load_model, load_vocoder, preprocess_ref_audio_text
from f5_tts.model import DiT
from huggingface_hub import hf_hub_download
from ruaccent import RUAccent

from phrases import PHRASES

# torchaudio ≥ 2.9 читает файлы через torchcodec, которому нужны DLL FFmpeg
# («shared»-сборка); f5-tts читает образец через torchaudio.load. Подменяем
# чтение на soundfile — WAV ему не нужен FFmpeg.
import torchaudio  # noqa: E402


def _load_wav(path, *args, **kwargs):
    data, rate = sf.read(str(path), dtype="float32", always_2d=True)
    return torch.from_numpy(data.T.copy()), rate


torchaudio.load = _load_wav

ROOT = Path(__file__).resolve().parents[2]
VOICE = ROOT / "tools" / "voice"
OUT = VOICE / "out"
F5 = OUT / "f5"
MODEL_REPO, MODEL_FILE = "ESpeech/ESpeech-TTS-1_RL-V2", "espeech_tts_rlv2.pt"
VOCAB_REPO = "ESpeech/ESpeech-TTS-1_podcaster"
MODEL_CFG = dict(dim=1024, depth=22, heads=16, ff_mult=2, text_dim=512, conv_layers=4)
NFE_STEPS = 48
REFERENCE_TEXT = "Запрос выполнен, сэр."


def load_stress() -> dict[str, str]:
    table = {}
    for line in (VOICE / "stress.txt").read_text(encoding="utf-8").splitlines():
        line = line.split("#", 1)[0].strip()
        if "=" in line:
            word, marked = (s.strip() for s in line.split("=", 1))
            table[word.lower()] = marked
    return table


class Stresser:
    def __init__(self) -> None:
        self.accent = RUAccent()
        self.accent.load(omograph_model_size="turbo3.1", use_dictionary=True, tiny_mode=False)
        self.table = load_stress()

    def accent_text(self, text: str) -> str:
        """RUAccent иногда падает на редких словах («Манхэттэн») — тогда
        по словам, а слово, на котором падает, остаётся без ударения."""
        try:
            return self.accent.process_all(text)
        except Exception:  # noqa: BLE001 — ошибка внутри onnx-модели RUAccent
            out = []
            for token in re.split(r"(\s+)", text):
                try:
                    out.append(self.accent.process_all(token) if token.strip() else token)
                except Exception:  # noqa: BLE001
                    out.append(token)
            return "".join(out)

    def __call__(self, text: str) -> str:
        if "+" not in text:
            text = self.accent_text(text)

        def fix(m: re.Match) -> str:
            word = m.group(0)
            plain = word.replace("+", "").lower()
            marked = self.table.get(plain)
            if marked is None:
                return word
            return marked.capitalize() if word[:1].isupper() else marked

        return re.sub(r"[А-Яа-яЁё+]+", fix, text)


def main() -> None:
    parser = argparse.ArgumentParser(description="Реплики голосом эталона (ESpeech)")
    parser.add_argument("text", nargs="?", help="своя фраза (без неё — все фразы phrases.py)")
    parser.add_argument("--id", default="custom")
    parser.add_argument("--takes", type=int, default=3)
    parser.add_argument("--ref", choices=["ref_short", "ref_long", "ref_dry", "all"], default="all")
    args = parser.parse_args()
    phrases = {args.id: args.text} if args.text else PHRASES

    stress = Stresser()
    refs_json = json.loads((OUT / "tts" / "references.json").read_text(encoding="utf-8"))
    long_text = ". ".join(t.strip() for t in refs_json["ref_long"]) + "."
    # «Сухой» эталон: выбранный select_best.py вариант (без реверберации).
    import csv

    with open(OUT / "report.csv", encoding="utf-8-sig") as f:
        etalon = next(r["name"] for r in csv.DictReader(f, delimiter=";") if r["file"] == "Запрос выполнен сэр")
    refs = {
        "ref_short": (OUT / "tts" / "ref_short.wav", REFERENCE_TEXT),
        "ref_long": (OUT / "tts" / "ref_long.wav", long_text),
        "ref_dry": (OUT / "clean" / f"{etalon}.wav", REFERENCE_TEXT),
    }
    if args.ref != "all":
        refs = {args.ref: refs[args.ref]}

    started = time.time()
    model = load_model(
        DiT,
        MODEL_CFG,
        hf_hub_download(MODEL_REPO, MODEL_FILE),
        vocab_file=hf_hub_download(VOCAB_REPO, "vocab.txt"),
    )
    vocoder = load_vocoder()
    print(f"Модель загружена за {time.time() - started:.1f} с", flush=True)

    timings = []
    for ref_name, (ref_wav, ref_text) in refs.items():
        ref_audio, ref_text = preprocess_ref_audio_text(str(ref_wav), stress(ref_text), show_info=lambda *_: None)
        for pid, text in phrases.items():
            out_dir = F5 / pid
            out_dir.mkdir(parents=True, exist_ok=True)
            marked = stress(text)
            (out_dir / "text.txt").write_text(text, encoding="utf-8")
            (out_dir / "stressed.txt").write_text(marked, encoding="utf-8")
            for take in range(1, args.takes + 1):
                target = out_dir / f"{ref_name}_{take}.wav"
                if target.exists():
                    continue
                torch.manual_seed(take)
                t = time.time()
                wave, rate, _ = infer_process(
                    ref_audio,
                    ref_text,
                    marked,
                    model,
                    vocoder,
                    nfe_step=NFE_STEPS,
                    show_info=lambda *_: None,
                    progress=None,
                )
                secs = time.time() - t
                sf.write(target, wave, rate)
                timings.append((secs, len(wave) / rate))
            print(f"  {ref_name} {pid}: {marked}", flush=True)
    if timings:
        synth = sum(t for t, _ in timings)
        audio = sum(a for _, a in timings)
        print(f"Синтез: {len(timings)} дублей, RTF {synth / audio:.2f} ({synth / len(timings):.2f} с на дубль)")


if __name__ == "__main__":
    main()
