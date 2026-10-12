"""Оценка качества реплик без эталонной чистой записи.

- DNSMOS (Microsoft, модель sig_bak_ovr.onnx, оценки 1–5):
  SIG — искажения самого голоса («дребезжание», «роботизированность»),
  BAK — посторонний фон, OVRL — общее впечатление.
- Похожесть тембра на эталонную реплику (CAM++, косинус): голос не должен
  уехать от того, как звучит лучшая запись.

Модель DNSMOS: tools/voice/models/dnsmos/sig_bak_ovr.onnx
(github.com/microsoft/DNS-Challenge, папка DNSMOS/DNSMOS).
"""

from __future__ import annotations

from pathlib import Path

import numpy as np
import onnxruntime as ort
import sherpa_onnx

ROOT = Path(__file__).resolve().parents[2]
DNSMOS = ROOT / "tools" / "voice" / "models" / "dnsmos" / "sig_bak_ovr.onnx"
SPEAKER = (
    ROOT / "models" / "speaker-campplus" / "3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx"
)
RATE = 16_000
SEGMENT = int(9.01 * RATE)

# Калибровка из dnsmos_local.py (не персонализированная).
P_SIG = np.poly1d([-0.08397278, 1.22083953, 0.0052439])
P_BAK = np.poly1d([-0.13166888, 1.60915514, -0.39604546])
P_OVR = np.poly1d([-0.06766283, 1.11546468, 0.04602535])


class Quality:
    def __init__(self) -> None:
        self.dnsmos = ort.InferenceSession(str(DNSMOS), providers=["CPUExecutionProvider"])
        config = sherpa_onnx.SpeakerEmbeddingExtractorConfig(model=str(SPEAKER), num_threads=4)
        self.speaker = sherpa_onnx.SpeakerEmbeddingExtractor(config)

    def mos(self, samples: np.ndarray) -> tuple[float, float, float]:
        """(SIG, BAK, OVRL) для моно 16 кГц. Короткие записи повторяются до 9 с,
        как в официальном скрипте."""
        audio = samples.astype(np.float32)
        while len(audio) < SEGMENT:
            audio = np.append(audio, audio)
        hops = int(np.floor(len(audio) / RATE) - 9.01) + 1
        scores = []
        for i in range(max(hops, 1)):
            seg = audio[i * RATE : i * RATE + SEGMENT]
            if len(seg) < SEGMENT:
                continue
            sig, bak, ovr = self.dnsmos.run(None, {"input_1": seg[np.newaxis, :]})[0][0]
            scores.append((P_SIG(sig), P_BAK(bak), P_OVR(ovr)))
        s = np.mean(scores, axis=0)
        return float(s[0]), float(s[1]), float(s[2])

    def embed(self, samples: np.ndarray) -> np.ndarray:
        stream = self.speaker.create_stream()
        stream.accept_waveform(RATE, samples.astype(np.float32))
        stream.input_finished()
        e = np.array(self.speaker.compute(stream), dtype=np.float32)
        return e / (np.linalg.norm(e) + 1e-9)
