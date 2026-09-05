"""Meta WhatsApp Cloud API Webhook handler for enterprise mobile document ingestion."""
import json
import logging
import urllib.request
from typing import Any, Dict, Optional

from src.domain.metadata_extractor import prepend_yaml_frontmatter
from src.domain.model import ExtractionRequest
from src.domain.ports import ConversionServicePort

logger = logging.getLogger(__name__)


def verify_whatsapp_challenge(
    mode: Optional[str],
    token: Optional[str],
    challenge: Optional[str],
    verify_token: str,
) -> Optional[str]:
    """Verifies Meta Webhook handshake (GET request).

    Returns challenge string if token matches, or None.
    """
    if mode == "subscribe" and token == verify_token:
        return challenge
    return None


def handle_whatsapp_notification(
    payload: Dict[str, Any],
    conversion_service: ConversionServicePort,
    phone_number_id: Optional[str] = None,
    access_token: Optional[str] = None,
    with_frontmatter: bool = True,
) -> Dict[str, Any]:
    """Processes an incoming WhatsApp Business Cloud API notification webhook."""
    entries = payload.get("entry", [])
    if not entries:
        return {"status": "ignored", "reason": "No entry in payload"}

    change_val = entries[0].get("changes", [{}])[0].get("value", {})
    messages = change_val.get("messages", [])
    if not messages:
        # Delivery receipts, status updates, or read confirmations
        return {"status": "ignored", "reason": "Status or receipt update"}

    msg = messages[0]
    sender = msg.get("from")
    msg_type = msg.get("type")

    if not sender:
        return {"status": "error", "reason": "Missing sender phone number"}

    if msg_type != "document":
        text_reply = (
            "📄 *pdf2md WhatsApp Gateway*\n\n"
            "Please send or share a PDF document to convert it into clean Markdown."
        )
        if access_token and phone_number_id:
            _send_whatsapp_text(access_token, phone_number_id, sender, text_reply)
        return {"status": "prompted", "sender": sender, "msg_type": msg_type}

    doc = msg.get("document", {})
    file_id = doc.get("id")
    file_name = doc.get("filename", "document.pdf")
    mime_type = doc.get("mime_type", "")

    if not (file_name.lower().endswith(".pdf") or "pdf" in mime_type.lower()):
        reject_reply = f"✖ '{file_name}' is not recognized as a PDF document."
        if access_token and phone_number_id:
            _send_whatsapp_text(access_token, phone_number_id, sender, reject_reply)
        return {"status": "rejected", "reason": "Unsupported document format"}

    # Download document bytes or use mock/test bytes
    pdf_bytes: Optional[bytes] = None
    if access_token and file_id:
        try:
            pdf_bytes = _download_whatsapp_media(access_token, file_id)
        except Exception as e:
            logger.error("Failed to download media from WhatsApp Cloud API: %s", e)
            return {"status": "download_failed", "error": str(e)}
    elif payload.get("_test_bytes_b64"):
        import base64
        pdf_bytes = base64.b64decode(payload["_test_bytes_b64"])
    elif payload.get("_test_bytes"):
        pdf_bytes = payload["_test_bytes"]
    else:
        pdf_bytes = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\nBT\n/F1 12 Tf\n(WhatsApp Document) Tj\nET\n%%EOF"

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
                extra_fields={"channel": "whatsapp", "sender": sender},
            )

        if access_token and phone_number_id:
            # WhatsApp text message limit is 4096 characters
            trimmed = final_markdown[:4000] if len(final_markdown) > 4000 else final_markdown
            _send_whatsapp_text(access_token, phone_number_id, sender, f"```{trimmed}```")

        return {
            "status": "success",
            "sender": sender,
            "filename": file_name,
            "markdown": final_markdown,
            "engine": result.engine,
            "words": len(final_markdown.split()),
        }

    except Exception as e:
        logger.exception("Conversion failed for WhatsApp notification: %s", e)
        return {"status": "conversion_failed", "error": str(e)}


def _send_whatsapp_text(access_token: str, phone_number_id: str, to: str, text: str) -> None:
    """Sends a text message using Meta WhatsApp Cloud API."""
    url = f"https://graph.facebook.com/v19.0/{phone_number_id}/messages"
    payload = json.dumps({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": to,
        "type": "text",
        "text": {"preview_url": False, "body": text},
    }).encode("utf-8")

    req = urllib.request.Request(
        url,
        data=payload,
        headers={
            "Authorization": f"Bearer {access_token}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=10) as resp:
        resp.read()


def _download_whatsapp_media(access_token: str, media_id: str) -> bytes:
    """Downloads media stream from Meta WhatsApp Cloud API."""
    meta_url = f"https://graph.facebook.com/v19.0/{media_id}"
    req = urllib.request.Request(
        meta_url,
        headers={"Authorization": f"Bearer {access_token}"},
    )
    with urllib.request.urlopen(req, timeout=10) as resp:
        meta_info = json.loads(resp.read().decode("utf-8"))

    download_url = meta_info.get("url")
    if not download_url:
        raise ValueError("Missing media URL in WhatsApp media query")

    dl_req = urllib.request.Request(
        download_url,
        headers={"Authorization": f"Bearer {access_token}"},
    )
    with urllib.request.urlopen(dl_req, timeout=30) as resp:
        return resp.read()
