"""Celery application configuration for asynchronous document conversion pipeline."""
import os
from celery import Celery

# Redis connection URL (default to local Redis instance)
REDIS_URL = os.environ.get("REDIS_URL", "redis://localhost:6379/0")
BROKER_URL = os.environ.get("CELERY_BROKER_URL", REDIS_URL)
RESULT_BACKEND = os.environ.get("CELERY_RESULT_BACKEND", REDIS_URL)

celery_app = Celery(
    "markdown_extract_service",
    broker=BROKER_URL,
    backend=RESULT_BACKEND,
    include=["src.infrastructure.queue.tasks"],
)

celery_app.conf.update(
    task_serializer="json",
    accept_content=["json"],
    result_serializer="json",
    timezone="UTC",
    enable_utc=True,
    task_track_started=True,
    task_time_limit=180,        # Hard timeout: 3 minutes per document
    task_soft_time_limit=120,   # Soft timeout: 2 minutes
    worker_prefetch_multiplier=1,
    worker_concurrency=int(os.environ.get("CELERY_CONCURRENCY", "2")),
    broker_connection_retry_on_startup=True,
)
