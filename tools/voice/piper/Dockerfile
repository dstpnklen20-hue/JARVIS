# Обучение голоса Piper (piper1-gpl) на видеокарте. Только для обучения —
# Джарвису Docker не нужен: готовый голос (.onnx) работает на процессоре.
#
# Сборка:  docker build -t jarvis-piper-train tools/voice/piper
# Запуск — tools/voice/piper/train.ps1
FROM python:3.11-slim-bookworm

RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential cmake ninja-build git \
 && rm -rf /var/lib/apt/lists/*

# PyTorch с CUDA 12.8 — первым: видеокарты RTX 50-й серии (sm_120) нужны сборки cu128.
RUN pip install --no-cache-dir torch==2.8.0 --index-url https://download.pytorch.org/whl/cu128

ARG PIPER_TAG=v1.8.0
RUN git clone --depth 1 --branch ${PIPER_TAG} https://github.com/OHF-Voice/piper1-gpl.git /piper
WORKDIR /piper
# setup.py build_ext запускается напрямую — инструменты сборки нужны в самом окружении.
RUN pip install --no-cache-dir scikit-build setuptools wheel cmake ninja \
 && pip install --no-cache-dir -e '.[train]' \
 && ./build_monotonic_align.sh \
 && python setup.py build_ext --inplace

# Данные, кэш и чекпоинты — в подключённой папке /work.
WORKDIR /work
