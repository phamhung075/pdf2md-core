"""ContextVars-aware logging system with per-request SSE log streaming."""
import contextvars
import logging
from typing import Any, Callable, Optional

from src.infrastructure.config import config

_current_log_callback: contextvars.ContextVar[Optional[Callable[[dict], Any]]] = contextvars.ContextVar(
    "current_log_callback", default=None
)


class ContextStreamLogHandler(logging.Handler):
    """Dispatches log records to the current thread/request's stream callback if active."""

    def emit(self, record: logging.LogRecord) -> None:
        cb = _current_log_callback.get()
        if cb is not None:
            try:
                cb({
                    "ts": round(record.created, 3),
                    "level": record.levelname,
                    "logger": record.name,
                    "message": record.getMessage(),
                })
            except Exception:
                pass


logger = logging.getLogger(config.service_name)


def set_log_callback(cb: Optional[Callable[[dict], Any]]) -> contextvars.Token:
    """Binds a streaming callback for the current context (thread/request)."""
    return _current_log_callback.set(cb)


def reset_log_callback(token: contextvars.Token) -> None:
    """Restores the previous streaming callback."""
    _current_log_callback.reset(token)


def init_logging() -> None:
    """Initializes console logging and the context stream handler."""
    formatter = logging.Formatter("%(asctime)s [%(levelname)s] %(name)s: %(message)s")

    console = logging.StreamHandler()
    console.setFormatter(formatter)

    stream_handler = ContextStreamLogHandler()
    stream_handler.setFormatter(formatter)

    root_logger = logging.getLogger()
    root_logger.setLevel(getattr(logging, config.log_level, logging.INFO))
    root_logger.handlers = [console, stream_handler]

    # Ensure RapidOCR propagates its logs to root
    logging.getLogger("RapidOCR").propagate = True
