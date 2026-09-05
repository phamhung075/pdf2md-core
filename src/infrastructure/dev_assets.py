"""Secure static asset server for the dev UI test page."""
import os
from typing import Dict, Optional, Tuple
from urllib.parse import unquote

from src.infrastructure.config import config

_DEV_MIME_TYPES: Dict[str, str] = {
    ".html": "text/html; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".mjs": "text/javascript; charset=utf-8",
    ".json": "application/json; charset=utf-8",
    ".svg": "image/svg+xml",
    ".png": "image/png",
    ".jpg": "image/jpeg",
    ".jpeg": "image/jpeg",
    ".ico": "image/x-icon",
}

_ASSET_CACHE: Dict[str, bytes] = {}


def get_dev_asset(request_path: str) -> Optional[Tuple[bytes, str]]:
    """Returns (bytes, content_type) for a /test asset, or None (dev UI off / unknown path).

    Assets live in dev_ui and are cached in memory. Path traversal is strictly forbidden.
    """
    if not config.dev_ui:
        return None

    if "\x00" in request_path:
        return None

    if request_path in ("/test", "/test/"):
        rel_path = "test.html"
    elif request_path in ("/test/compare", "/test/compare/"):
        rel_path = "compare.html"
    elif request_path.startswith("/test/"):
        rel_path = unquote(request_path[len("/test/"):].split("?")[0])
    else:
        return None

    if "\x00" in rel_path:
        return None

    # Disallow absolute paths or directory traversal attempts
    clean_rel = os.path.normpath(rel_path).lstrip("/\\")
    if clean_rel.startswith(".."):
        return None

    # Resolve symlinks and canonicalize paths to prevent symlink traversal
    ui_dir_real = os.path.realpath(config.dev_ui_dir)
    target_candidate = os.path.join(ui_dir_real, clean_rel)
    full_path = os.path.realpath(target_candidate)

    if not (full_path == ui_dir_real or full_path.startswith(ui_dir_real + os.sep)):
        return None

    if not os.path.isfile(full_path):
        return None

    ext = os.path.splitext(full_path)[1].lower()
    content_type = _DEV_MIME_TYPES.get(ext, "application/octet-stream")

    if clean_rel not in _ASSET_CACHE:
        try:
            with open(full_path, "rb") as f:
                _ASSET_CACHE[clean_rel] = f.read()
        except OSError:
            return None

    return _ASSET_CACHE[clean_rel], content_type
