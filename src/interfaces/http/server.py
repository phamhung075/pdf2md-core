import signal
import threading
import time
from http.server import ThreadingHTTPServer

from src.infrastructure.config import config
from src.infrastructure.converters.docling_pipeline import get_converter, warmup
from src.infrastructure.logging.stream_logger import init_logging, logger
from src.interfaces.http.handler import ExtractServiceHandler


def run_server() -> None:
    """Initializes logging, pre-warms models, and starts the threaded HTTP server."""
    init_logging()

    # Pre-load Docling models and neural network weights synchronously so first request never lags or races.
    logger.info("Initializing and pre-warming Docling pipeline (Heron layout + TableFormer models)...")
    t_start = time.monotonic()
    warmup()
    logger.info("Docling pipeline warm-up completed in %.2f sec.", time.monotonic() - t_start)

    server = ThreadingHTTPServer((config.host, config.port), ExtractServiceHandler)

    display_host = "127.0.0.1" if config.host == "0.0.0.0" else config.host
    dev_ui_msg = f", dev test page http://{display_host}:{config.port}/test" if config.dev_ui else ""
    logger.info(
        "%s listening on http://%s:%d — POST /extract (?stream=1 for SSE) or /to-markdown, GET /health%s",
        config.service_name,
        display_host,
        config.port,
        dev_ui_msg,
    )

    def _shutdown(signum, frame):
        logger.info("Received signal %s; shutting down %s...", signum, config.service_name)
        threading.Thread(target=server.shutdown, daemon=True).start()

    try:
        signal.signal(signal.SIGINT, _shutdown)
        signal.signal(signal.SIGTERM, _shutdown)
    except (ValueError, AttributeError):
        pass  # Running in an environment where signal handlers are restricted

    try:
        server.serve_forever()
    finally:
        server.server_close()
        logger.info("%s stopped.", config.service_name)
