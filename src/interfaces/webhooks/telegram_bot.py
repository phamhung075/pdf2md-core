"""Stateless Telegram Bot Webhook handler for instant mobile PDF-to-Markdown conversion."""
import io
import json
import logging
import urllib.parse
import urllib.request
from typing import Any, Dict, Optional

from src.domain.model import ExtractionRequest
from src.domain.metadata_extractor import prepend_yaml_frontmatter
from src.domain.ports import ConversionServicePort

logger = logging.getLogger(__name__)

MAX_TELEGRAM_MSG_LEN = 4000


def handle_telegram_update(
    update: Dict[str, Any],
    conversion_service: ConversionServicePort,
    bot_token: Optional[str] = None,
    with_frontmatter: bool = True,
) -> Dict[str, Any]:
    """Processes an incoming Telegram Bot API Update webhook payload.

    Returns a structured summary dictionary with conversion status and response message.
    """
    message = update.get("message") or update.get("edited_message")
    if not message:
        return {"status": "ignored", "reason": "No message field in update"}

    chat_id = message.get("chat", {}).get("id")
    if not chat_id:
        return {"status": "error", "reason": "Missing chat.id"}

    text = (message.get("text") or "").strip()

    # 1. Handle commands: /start, /help
    if text.startswith("/start") or text.startswith("/help"):
        welcome_text = (
            "👋 *Welcome to pdf2md Bot!*\n\n"
            "Send or forward any PDF document directly to this chat.\n"
            "I will extract clean *GitHub Flavored Markdown* and reconstruct your tables instantly.\n\n"
            "⚡ Powered by the sub-millisecond Rust native core (`pdf2md-core`)."
        )
        if bot_token:
            _send_telegram_message(bot_token, chat_id, welcome_text)
        return {
            "status": "command_handled",
            "chat_id": chat_id,
            "response": welcome_text,
        }

    # 2. Handle document attachment
    doc = message.get("document")
    if not doc:
        hint_text = "Please send a PDF document file to convert it into Markdown."
        if bot_token:
            _send_telegram_message(bot_token, chat_id, hint_text)
        return {"status": "ignored", "reason": "Message is not a document"}

    file_name = doc.get("file_name", "document.pdf")
    mime_type = doc.get("mime_type", "")
    file_id = doc.get("file_id")

    if not (file_name.lower().endswith(".pdf") or "pdf" in mime_type.lower()):
        reject_text = f"✖ Sorry, '{file_name}' is not recognized as a PDF document."
        if bot_token:
            _send_telegram_message(bot_token, chat_id, reject_text)
        return {"status": "rejected", "reason": "Unsupported document format"}

    # 3. Download document bytes (or use mock/test payload if token absent)
    pdf_bytes: Optional[bytes] = None
    if bot_token and file_id:
        try:
            pdf_bytes = _download_telegram_file(bot_token, file_id)
        except Exception as e:
            logger.error("Failed to download file from Telegram API: %s", e)
            error_text = f"✖ Could not download document from Telegram: {e}"
            _send_telegram_message(bot_token, chat_id, error_text)
            return {"status": "download_failed", "error": str(e)}
    elif update.get("_test_bytes_b64"):
        import base64
        pdf_bytes = base64.b64decode(update["_test_bytes_b64"])
    elif update.get("_test_bytes"):
        pdf_bytes = update["_test_bytes"]
    else:
        # Dry-run / mock simulation when testing webhook without network token
        pdf_bytes = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\nBT\n/F1 12 Tf\n(Mock Document) Tj\nET\n%%EOF"

    # 4. Execute extraction pipeline
    try:
        request = ExtractionRequest(
            content=pdf_bytes,
            filename=file_name,
            embed_images=True,
            allow_fast_path=True,
        )
        converter_fn = getattr(conversion_service, "convert_request", getattr(conversion_service, "convert", None))
        result = converter_fn(request)
        final_markdown = result.markdown

        if with_frontmatter:
            final_markdown = prepend_yaml_frontmatter(
                final_markdown,
                filename=file_name,
                extra_fields={"channel": "telegram", "chat_id": chat_id},
            )

        # 5. Reply to Telegram user
        if bot_token:
            if len(final_markdown) <= MAX_TELEGRAM_MSG_LEN:
                _send_telegram_message(bot_token, chat_id, f"```markdown\n{final_markdown}\n```")
            else:
                summary = (
                    f"✔ *Conversion Complete: {file_name}*\n"
                    f"Output exceeds Telegram's 4,096-char message limit ({len(final_markdown)} characters).\n"
                    "Sending as Markdown file attachment below..."
                )
                _send_telegram_message(bot_token, chat_id, summary)
                out_name = file_name.rsplit(".", 1)[0] + ".md"
                _send_telegram_document(bot_token, chat_id, final_markdown.encode("utf-8"), out_name)

        return {
            "status": "success",
            "chat_id": chat_id,
            "filename": file_name,
            "markdown": final_markdown,
            "engine": result.engine,
            "words": len(final_markdown.split()),
        }

    except Exception as e:
        logger.exception("Conversion failed for Telegram update: %s", e)
        if bot_token:
            _send_telegram_message(bot_token, chat_id, f"✖ Conversion error: {e}")
        return {"status": "conversion_failed", "error": str(e)}


def _send_telegram_message(bot_token: str, chat_id: Any, text: str) -> None:
    """Sends a text message using the Telegram Bot API."""
    url = f"https://api.telegram.org/bot{bot_token}/sendMessage"
    payload = json.dumps({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "Markdown",
        "disable_web_page_preview": True,
    }).encode("utf-8")

    req = urllib.request.Request(
        url,
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=10) as resp:
        resp.read()


def _send_telegram_document(bot_token: str, chat_id: Any, content: bytes, filename: str) -> None:
    """Sends a document attachment using multipart form-data."""
    boundary = "----TelegramBotBoundary" + str(hash(filename))
    body = io.BytesIO()

    # chat_id field
    body.write(f"--{boundary}\r\n".encode("utf-8"))
    body.write(b'Content-Disposition: form-data; name="chat_id"\r\n\r\n')
    body.write(f"{chat_id}\r\n".encode("utf-8"))

    # document field
    body.write(f"--{boundary}\r\n".encode("utf-8"))
    body.write(f'Content-Disposition: form-data; name="document"; filename="{filename}"\r\n'.encode("utf-8"))
    body.write(b"Content-Type: text/markdown\r\n\r\n")
    body.write(content)
    body.write(b"\r\n")
    body.write(f"--{boundary}--\r\n".encode("utf-8"))

    payload = body.getvalue()
    url = f"https://api.telegram.org/bot{bot_token}/sendDocument"
    req = urllib.request.Request(
        url,
        data=payload,
        headers={"Content-Type": f"multipart/form-data; boundary={boundary}"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=15) as resp:
        resp.read()


def _download_telegram_file(bot_token: str, file_id: str) -> bytes:
    """Queries Telegram getFile API and downloads the raw file stream."""
    url = f"https://api.telegram.org/bot{bot_token}/getFile?file_id={file_id}"
    req = urllib.request.Request(url)
    with urllib.request.urlopen(req, timeout=10) as resp:
        info = json.loads(resp.read().decode("utf-8"))

    if not info.get("ok") or "result" not in info:
        raise ValueError(f"Telegram getFile error: {info}")

    file_path = info["result"].get("file_path")
    if not file_path:
        raise ValueError("Missing file_path in Telegram getFile response")

    download_url = f"https://api.telegram.org/file/bot{bot_token}/{file_path}"
    with urllib.request.urlopen(download_url, timeout=30) as resp:
        return resp.read()
