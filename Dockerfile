# syntax=docker/dockerfile:1
# markdown-extract-service image (see README.md and docker-compose.yml in this project).
#
# Model strategy — baked at BUILD time, offline at runtime:
#   - RapidOCR PP-OCRv6 onnx models ship inside the rapidocr wheel (pip layer below).
#   - The layout/table models (~500 MB from Hugging Face) download when the pipeline warm-up runs.
#     Layer order matters: requirements + docling_pipeline.py (stable) are COPYed and warmed BEFORE
#     main.py (the code you actually edit) — so editing the server code never re-downloads models.
#
# python:3.12-slim matches the environment the whole Docling evaluation ran on.

FROM python:3.12-slim

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PYTHONPATH=/app \
    HF_HOME=/app/models \
    HF_HUB_CACHE=/app/models/hub \
    XDG_CACHE_HOME=/app/cache \
    TOKENIZERS_PARALLELISM=false

# onnxruntime (CPU) needs libgomp at runtime; slim images do not ship it.
# RapidOCR runs entirely in Python via ONNX Runtime (bundled PP-OCRv6 models).
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libgomp1 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -g 10001 appgroup \
    && useradd -u 10001 -g appgroup -s /bin/sh -m appuser

WORKDIR /app

COPY requirements.txt ./
# rapidocr (docling[rapidocr]) requires the GUI `opencv-python` dist, but its 4.14+/5.x wheels
# link Qt/X11 (libxcb, libGL) and cannot even be imported on slim. Swap in
# opencv-python-headless (same version). Order matters: uninstall the GUI wheel FIRST — both
# dists ship identical cv2/ files, so uninstalling it afterwards would delete headless cv2 too.
# BuildKit pip cache mount preserves downloaded wheels across builds.
# Install CPU-only PyTorch first to prevent pulling ~6 GB of unused NVIDIA CUDA GPU binaries on slim CPU images.
RUN --mount=type=cache,target=/root/.cache/pip \
    pip install --index-url https://download.pytorch.org/whl/cpu torch torchvision \
    && pip install --extra-index-url https://download.pytorch.org/whl/cpu -r requirements.txt \
    && pip uninstall -y opencv-python \
    && pip install opencv-python-headless==5.0.0.93 \
    && python -c "import cv2, onnxruntime"   # fail the build fast if the OCR stack cannot import

# Copy docling_pipeline and warm up layout/table models (cached layer).
# Mount /tmp/hf_cache so layout & table models (~500 MB) are downloaded only once and reused on rebuilds.
COPY src/infrastructure/converters/docling_pipeline.py ./src/infrastructure/converters/
RUN --mount=type=cache,target=/tmp/hf_cache python - <<'EOF'
import os
import shutil

cache_dir = '/tmp/hf_cache'
target_dir = '/app/models/hub'
os.makedirs(cache_dir, exist_ok=True)
os.makedirs(target_dir, exist_ok=True)

if os.path.exists(cache_dir) and os.listdir(cache_dir):
    shutil.copytree(cache_dir, target_dir, dirs_exist_ok=True)

from src.infrastructure.converters.docling_pipeline import warmup
warmup()

shutil.copytree(target_dir, cache_dir, dirs_exist_ok=True)
EOF

# Application, domain, infrastructure, and interface code AFTER warm-up
COPY --chown=appuser:appgroup src/ ./src/
COPY --chown=appuser:appgroup main.py run_mcp.py ./
COPY --chown=appuser:appgroup dev_ui/ ./dev_ui/

# Ensure all cache and model paths are owned and readable by non-root appuser
RUN chown -R appuser:appgroup /app && chmod -R a+rX /app

USER appuser

# The host-facing default (127.0.0.1) lives in main.py; the container overrides to 0.0.0.0 so the
# published port can reach it.
ENV DOCLING_SERVICE_HOST=0.0.0.0 \
    DOCLING_SERVICE_PORT=3984

EXPOSE 3984

CMD ["python", "main.py"]
