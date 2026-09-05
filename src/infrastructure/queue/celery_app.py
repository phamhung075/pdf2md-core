"""Celery application configuration for asynchronous document conversion pipeline with multi-queue priority routing."""
import os
from celery import Celery
from kombu import Exchange, Queue

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

default_exchange = Exchange("default", type="direct")

task_queues = (
    Queue("queue:interactive", exchange=default_exchange, routing_key="queue.interactive"),
    Queue("queue:batch", exchange=default_exchange, routing_key="queue.batch"),
    Queue("queue:dead_letter", exchange=default_exchange, routing_key="queue.dead_letter"),
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
    task_queues=task_queues,
    task_default_queue="queue:batch",
    task_default_exchange="default",
    task_default_routing_key="queue.batch",
    task_routes={
        "tasks.convert_document_interactive": {"queue": "queue:interactive", "routing_key": "queue.interactive"},
        "tasks.convert_document_batch": {"queue": "queue:batch", "routing_key": "queue.batch"},
        "tasks.dead_letter": {"queue": "queue:dead_letter", "routing_key": "queue.dead_letter"},
    },
)
