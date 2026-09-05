"""Docling pipeline factory shared by the server and the Docker build warm-up.

Kept free of any server/HTTP imports so the image build can COPY just this module, run the model
warm-up against it (downloads layout/table models at BUILD time into /app/models), and only then
COPY the server code — a code edit never re-downloads the ~500 MB of models.
"""
import os

_converter = None


def get_converter():
    """One DocumentConverter for every supported format.

    .pdf gets the full pipeline — layout (Heron) + table structure (TableFormer) + RapidOCR
    (PP-OCRv6, fr). Every other extension is handled by Docling's own native readers (the pdf
    format option is simply irrelevant to them), so office files convert with no OCR and no layout
    models.
    """
    global _converter
    if _converter is not None:
        return _converter
    from docling.datamodel.pipeline_options import PdfPipelineOptions, RapidOcrOptions  # type: ignore
    from docling.document_converter import DocumentConverter, ImageFormatOption, PdfFormatOption  # type: ignore

    opts = PdfPipelineOptions()
    opts.do_ocr = True
    opts.do_table_structure = True
    opts.do_code_enrichment = False
    opts.do_formula_enrichment = False
    opts.generate_page_images = False
    opts.generate_picture_images = os.environ.get("DOCLING_EMBED_IMAGES", "1").strip().lower() in ("1", "true", "yes", "on")
    opts.ocr_options = RapidOcrOptions()

    _converter = DocumentConverter(
        format_options={
            "pdf": PdfFormatOption(pipeline_options=opts),
            "image": ImageFormatOption(pipeline_options=opts),
        }
    )
    return _converter


import io
import logging
import time

logger = logging.getLogger("docling_pipeline")


def warmup() -> None:
    """Pre-warm Docling pipeline models on server init / Docker build stage.

    Ensures all Hugging Face weights are downloaded to local cache and loaded
    into memory, eliminating cold-start latency on the first conversion request.
    """
    t0 = time.monotonic()
    conv = get_converter()

    # 1. Pre-download / verify Hugging Face model snapshots to local cache directory
    try:
        from docling.models.stages.table_structure.table_structure_model import TableStructureModel  # type: ignore
        TableStructureModel.download_models()
    except Exception as exc:
        logger.warning("TableStructureModel pre-download notice: %s", exc)

    try:
        from docling.datamodel.pipeline_options import LayoutObjectDetectionOptions  # type: ignore
        from docling.models.utils.hf_model_download import download_hf_model  # type: ignore
        layout_opts = LayoutObjectDetectionOptions()
        download_hf_model(layout_opts.model_spec.repo_id, revision=layout_opts.model_spec.revision)
    except Exception as exc:
        logger.warning("Layout model pre-download notice: %s", exc)

    # 2. Run a synthetic table conversion in-memory to load Heron + TableFormer weights into PyTorch memory
    try:
        import pymupdf  # type: ignore
        from docling.datamodel.base_models import DocumentStream  # type: ignore

        doc = pymupdf.open()
        page = doc.new_page(width=400, height=300)
        page.draw_rect(pymupdf.Rect(50, 50, 350, 150), color=(0, 0, 0), width=1)
        page.draw_line(pymupdf.Point(50, 80), pymupdf.Point(350, 80), color=(0, 0, 0), width=1)
        page.draw_line(pymupdf.Point(150, 50), pymupdf.Point(150, 150), color=(0, 0, 0), width=1)
        page.insert_text((60, 70), "Col 1")
        page.insert_text((160, 70), "Col 2")
        page.insert_text((60, 110), "Val 1")
        page.insert_text((160, 110), "Val 2")
        pdf_bytes = doc.tobytes()
        doc.close()

        conv.convert(DocumentStream(name="warmup_init.pdf", stream=io.BytesIO(pdf_bytes)))
        elapsed = round(time.monotonic() - t0, 2)
        logger.info("Docling pipeline warm-up completed in %s sec.", elapsed)
    except Exception as exc:
        logger.warning("Docling pipeline synthetic conversion notice: %s", exc)

