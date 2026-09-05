import { $, escapeHtml, sanitizeHtml } from './infra/dom.js';
import { loadPdfJs } from './infra/pdfjs-loader.js';
import { loadCompareData, onCompareDataUpdated, saveCompareData } from './infra/compare-sync.js';

let pdfDoc = null;
let pdfBytes = null;
let currentPdfUrl = null;
let lastMarkdown = '';
let currentScale = 'fit';
let mdFontSize = 14;
let isSyncingScroll = false;
let isDraggingResizer = false;

const IMAGE_EXTENSIONS = ['.jpg', '.jpeg', '.png', '.webp', '.bmp', '.tiff', '.tif'];
function isImageBlobOrName(blob, name) {
  if (blob && blob.type && blob.type.startsWith('image/')) return true;
  const n = (name || '').toLowerCase();
  return IMAGE_EXTENSIONS.some((ext) => n.endsWith(ext));
}

// DOM Elements
const workspace = $('cmp-workspace');
const panePdf = $('cmp-pane-pdf');
const resizer = $('cmp-resizer');
const pdfViewport = $('cmp-pdf-viewport');
const pdfPagesList = $('cmp-pdf-pages-list');
const mdViewport = $('cmp-md-viewport');
const mdContent = $('cmp-md-content');
const rawMdPre = $('cmp-raw-md');
const filenameEl = $('cmp-filename');
const metaBadgesEl = $('cmp-meta-badges');
const pageIndicator = $('cmp-page-indicator');
const charsCountEl = $('cmp-chars-count');
const syncScrollCb = $('cmp-sync-scroll');
const rawPdfLink = $('cmp-pdf-raw-link');
const fileInput = $('cmp-file-input');

async function renderPdf(targetScale) {
  if (!pdfDoc) return;
  try {
    pdfPagesList.innerHTML = '<div class="empty" style="padding:40px;color:var(--muted);">Rendering PDF pages…</div>';
    const totalPages = pdfDoc.numPages;
    const containerWidth = pdfViewport.clientWidth || 600;
    const availableWidth = Math.max(300, containerWidth - 48);

    pdfPagesList.innerHTML = '';

    for (let p = 1; p <= totalPages; p++) {
      const page = await pdfDoc.getPage(p);
      const unscaledViewport = page.getViewport({ scale: 1 });

      let scale;
      if (targetScale === 'fit') {
        scale = availableWidth / unscaledViewport.width;
      } else {
        scale = targetScale;
      }

      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const viewport = page.getViewport({ scale: scale * dpr });

      const card = document.createElement('div');
      card.className = 'pdf-page-card';
      card.id = `cmp-page-${p}`;
      card.dataset.page = p;

      if (totalPages > 1) {
        const numLabel = document.createElement('div');
        numLabel.className = 'pdf-page-number';
        numLabel.textContent = `Page ${p} of ${totalPages}`;
        card.appendChild(numLabel);
      }

      const canvas = document.createElement('canvas');
      canvas.className = 'pdf-page-canvas';
      canvas.width = viewport.width;
      canvas.height = viewport.height;
      canvas.style.width = `${Math.round(viewport.width / dpr)}px`;
      canvas.style.height = `${Math.round(viewport.height / dpr)}px`;

      const ctx = canvas.getContext('2d');
      await page.render({ canvasContext: ctx, viewport }).promise;

      card.appendChild(canvas);
      pdfPagesList.appendChild(card);
    }

    pageIndicator.textContent = `1 / ${totalPages}`;
  } catch (err) {
    console.error('Render PDF failed:', err);
    pdfPagesList.innerHTML = `<div class="empty" style="color:var(--err);padding:24px;">Failed rendering PDF: ${escapeHtml(err.message)}</div>`;
  }
}

function renderMarkdown(md) {
  lastMarkdown = md || '';
  charsCountEl.textContent = `${lastMarkdown.length.toLocaleString()} chars`;
  rawMdPre.textContent = lastMarkdown || '(no markdown content)';

  if (!lastMarkdown) {
    mdContent.innerHTML = `
      <div class="compare-empty-banner">
        <h3>No Markdown content</h3>
        <p>Run a conversion in the dev test page to compare markdown with the original PDF.</p>
      </div>`;
    return;
  }

  try {
    const raw = window.marked ? window.marked.parse(lastMarkdown) : lastMarkdown;
    mdContent.innerHTML = sanitizeHtml(raw);
  } catch (e) {
    mdContent.innerHTML = `<pre class="empty" style="color:var(--err);">Markdown parsing error: ${escapeHtml(e.message)}</pre>`;
  }
}

let currentFileName = '';

function loadFromOpenerIfAvailable() {
  if (window.opener && !window.opener.closed) {
    try {
      if (typeof window.opener.getComparePayload === 'function') {
        const payload = window.opener.getComparePayload();
        if (payload && (payload.fileBlob || payload.file || payload.markdown || payload.fileName)) {
          return payload;
        }
      }
    } catch (e) {
      console.warn('Unable to access window.opener payload:', e);
    }
  }
  return null;
}

async function applyComparePayload(data) {
  if (!data) return;

  const newFileName = data.fileName || '';
  if (newFileName) {
    filenameEl.textContent = newFileName;
    document.title = `${newFileName} — Side-by-Side`;
  }

  // Render metadata badges
  metaBadgesEl.innerHTML = '';
  if (data.metadata) {
    const m = data.metadata;
    if (m.engine) addBadge('Engine', m.engine);
    if (m.numpages) addBadge('Pages', m.numpages);
    if (m.duration_ms != null) addBadge('Duration', `${m.duration_ms} ms`);
  }

  if (data.markdown != null) {
    renderMarkdown(data.markdown);
  }

  const blob = data.fileBlob || (data.file instanceof Blob ? data.file : null) ||
    (data.pdfBytes && data.pdfBytes.byteLength > 0 ? new Blob([data.pdfBytes], { type: 'application/pdf' }) : null);

  if (blob) {
    const fileChanged = !pdfDoc || (newFileName && newFileName !== currentFileName);
    if (fileChanged) {
      currentFileName = newFileName;
      if (currentPdfUrl) URL.revokeObjectURL(currentPdfUrl);
      currentPdfUrl = URL.createObjectURL(blob);
      rawPdfLink.href = currentPdfUrl;

      if (isImageBlobOrName(blob, newFileName)) {
        pdfDoc = null;
        pageIndicator.textContent = '1 / 1';
        pdfPagesList.innerHTML = `
          <div class="pdf-page-card" style="display:flex;justify-content:center;align-items:center;padding:16px;">
            <img src="${currentPdfUrl}" alt="${escapeHtml(newFileName)}" style="max-width:100%;height:auto;border-radius:4px;box-shadow:0 2px 10px rgba(0,0,0,0.12);" />
          </div>`;
        return;
      }

      try {
        const pdfjs = await loadPdfJs();
        const arrayBuffer = await blob.arrayBuffer();
        pdfDoc = await pdfjs.getDocument({ data: new Uint8Array(arrayBuffer) }).promise;
        pageIndicator.textContent = `1 / ${pdfDoc.numPages}`;
        await renderPdf(currentScale);
      } catch (err) {
        console.error('Failed to parse and render PDF in compare view:', err);
        pdfPagesList.innerHTML = `<div class="empty" style="color:var(--err);padding:24px;">Failed rendering PDF: ${escapeHtml(err.message)}</div>`;
      }
    }
  }
}

function addBadge(label, val) {
  const span = document.createElement('span');
  span.className = 'chip';
  span.style.padding = '2px 6px';
  span.innerHTML = `<span style="color:var(--muted);">${escapeHtml(label)}:</span> <b>${escapeHtml(String(val))}</b>`;
  metaBadgesEl.appendChild(span);
}

// Synchronized scrolling logic
function setupScrollSync() {
  let isPdfScrolling = false;
  let isMdScrolling = false;

  pdfViewport.addEventListener('scroll', () => {
    if (!syncScrollCb.checked || isMdScrolling) return;
    isPdfScrolling = true;

    // Track active page indicator
    if (pdfDoc) {
      const cards = pdfPagesList.querySelectorAll('.pdf-page-card');
      const middle = pdfViewport.scrollTop + 80;
      for (const c of cards) {
        if (c.offsetTop <= middle && (c.offsetTop + c.offsetHeight) > middle) {
          pageIndicator.textContent = `${c.dataset.page} / ${pdfDoc.numPages}`;
          break;
        }
      }
    }

    const maxPdf = pdfViewport.scrollHeight - pdfViewport.clientHeight;
    if (maxPdf > 0) {
      const ratio = pdfViewport.scrollTop / maxPdf;
      const maxMd = mdViewport.scrollHeight - mdViewport.clientHeight;
      mdViewport.scrollTop = ratio * maxMd;
    }

    setTimeout(() => { isPdfScrolling = false; }, 60);
  }, { passive: true });

  mdViewport.addEventListener('scroll', () => {
    if (!syncScrollCb.checked || isPdfScrolling) return;
    isMdScrolling = true;

    const maxMd = mdViewport.scrollHeight - mdViewport.clientHeight;
    if (maxMd > 0) {
      const ratio = mdViewport.scrollTop / maxMd;
      const maxPdf = pdfViewport.scrollHeight - pdfViewport.clientHeight;
      pdfViewport.scrollTop = ratio * maxPdf;
    }

    setTimeout(() => { isMdScrolling = false; }, 60);
  }, { passive: true });
}

// Draggable split bar
function setupResizer() {
  resizer.addEventListener('mousedown', (e) => {
    isDraggingResizer = true;
    resizer.classList.add('dragging');
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
  });

  window.addEventListener('mousemove', (e) => {
    if (!isDraggingResizer) return;
    const totalW = workspace.clientWidth;
    const minW = 260;
    const newPdfW = Math.max(minW, Math.min(totalW - minW, e.clientX));
    panePdf.style.width = `${newPdfW}px`;
    panePdf.style.flex = 'none';
  });

  window.addEventListener('mouseup', () => {
    if (isDraggingResizer) {
      isDraggingResizer = false;
      resizer.classList.remove('dragging');
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      // Re-render PDF to fit the newly resized column if in 'fit' mode
      if (currentScale === 'fit') {
        renderPdf('fit');
      }
    }
  });

  // Double click to reset to 50/50
  resizer.addEventListener('dblclick', () => {
    panePdf.style.width = '50%';
    panePdf.style.flex = '';
    if (currentScale === 'fit') renderPdf('fit');
  });

  $('cmp-split-reset').addEventListener('click', () => {
    panePdf.style.width = '50%';
    panePdf.style.flex = '';
    if (currentScale === 'fit') renderPdf('fit');
  });
}

// Zoom controls
function setupZoomControls() {
  $('cmp-zoom-in').addEventListener('click', () => {
    let cur = (typeof currentScale === 'number') ? currentScale : 1.0;
    currentScale = Math.min(3.5, Math.round((cur + 0.25) * 100) / 100);
    $('cmp-zoom-reset').textContent = `${Math.round(currentScale * 100)}%`;
    renderPdf(currentScale);
  });

  $('cmp-zoom-out').addEventListener('click', () => {
    let cur = (typeof currentScale === 'number') ? currentScale : 1.0;
    currentScale = Math.max(0.35, Math.round((cur - 0.25) * 100) / 100);
    $('cmp-zoom-reset').textContent = `${Math.round(currentScale * 100)}%`;
    renderPdf(currentScale);
  });

  $('cmp-zoom-reset').addEventListener('click', () => {
    currentScale = 'fit';
    $('cmp-zoom-reset').textContent = 'Fit';
    renderPdf('fit');
  });
}

// Markdown controls (font sizing, copy, raw toggle)
function setupMarkdownControls() {
  const viewToggle = $('cmp-view-toggle');
  viewToggle.addEventListener('click', () => {
    const isRaw = rawMdPre.style.display !== 'none';
    if (isRaw) {
      rawMdPre.style.display = 'none';
      mdContent.style.display = 'block';
      viewToggle.textContent = 'Show Raw MD';
    } else {
      rawMdPre.style.display = 'block';
      mdContent.style.display = 'none';
      viewToggle.textContent = 'Show Rendered';
    }
  });

  $('cmp-font-inc').addEventListener('click', () => {
    mdFontSize = Math.min(22, mdFontSize + 1);
    mdContent.style.fontSize = `${mdFontSize}px`;
  });

  $('cmp-font-dec').addEventListener('click', () => {
    mdFontSize = Math.max(11, mdFontSize - 1);
    mdContent.style.fontSize = `${mdFontSize}px`;
  });

  const copyBtn = $('cmp-copy-md');
  copyBtn.addEventListener('click', () => {
    if (!lastMarkdown) return;
    navigator.clipboard.writeText(lastMarkdown).then(() => {
      const orig = copyBtn.textContent;
      copyBtn.textContent = 'Copied!';
      setTimeout(() => { copyBtn.textContent = orig; }, 1500);
    });
  });
}

// Fullscreen toggle
function setupFullscreen() {
  const fsBtn = $('cmp-fullscreen-btn');
  fsBtn.addEventListener('click', () => {
    if (!document.fullscreenElement) {
      document.documentElement.requestFullscreen().catch(() => {});
      fsBtn.textContent = '✕ Exit Full';
    } else {
      document.exitFullscreen().catch(() => {});
      fsBtn.textContent = '⛶ Fullscreen';
    }
  });

  document.addEventListener('fullscreenchange', () => {
    if (!document.fullscreenElement) {
      fsBtn.textContent = '⛶ Fullscreen';
    }
  });
}

// Direct file loading
function setupDirectFileLoad() {
  $('cmp-choose-btn').addEventListener('click', () => fileInput.click());
  const dropZone = $('cmp-drop-zone');
  if (dropZone) dropZone.addEventListener('click', () => fileInput.click());

  fileInput.addEventListener('change', async () => {
    const file = fileInput.files[0];
    if (!file) return;

    await applyComparePayload({
      fileName: file.name,
      fileBlob: file,
      markdown: lastMarkdown,
      metadata: { engine: 'manual' },
    });

    saveCompareData({
      fileName: file.name,
      fileBlob: file,
      markdown: lastMarkdown,
      metadata: { engine: 'manual', numpages: pdfDoc?.numPages },
    });
  });

  // Drag & drop support
  window.addEventListener('dragover', (e) => e.preventDefault());
  window.addEventListener('drop', async (e) => {
    e.preventDefault();
    if (e.dataTransfer && e.dataTransfer.files.length) {
      const file = e.dataTransfer.files[0];
      if (file.type === 'application/pdf' || file.name.endsWith('.pdf') || isImageBlobOrName(file, file.name)) {
        fileInput.files = e.dataTransfer.files;
        fileInput.dispatchEvent(new Event('change'));
      }
    }
  });
}

// Window resize handler
window.addEventListener('resize', () => {
  if (currentScale === 'fit' && pdfDoc) {
    renderPdf('fit');
  }
});

// Init on load
document.addEventListener('DOMContentLoaded', async () => {
  setupResizer();
  setupScrollSync();
  setupZoomControls();
  setupMarkdownControls();
  setupFullscreen();
  setupDirectFileLoad();

  let loaded = false;
  // 1. First priority: Try instant handshake with opener
  const openerPayload = loadFromOpenerIfAvailable();
  if (openerPayload && (openerPayload.fileBlob || openerPayload.file || openerPayload.markdown)) {
    console.log('Loaded compare payload directly from window.opener');
    await applyComparePayload(openerPayload);
    loaded = true;
  }

  // 2. Second priority / sync: Load from IndexedDB
  const dbData = await loadCompareData();
  if (dbData) {
    if (!loaded || (dbData.metadata && !openerPayload?.metadata)) {
      console.log('Loaded compare payload from IndexedDB');
      await applyComparePayload(dbData);
    }
  }

  // 3. Listen for live updates from test.html via BroadcastChannel
  onCompareDataUpdated(async (evt) => {
    console.log('Received compare data update from channel:', evt);
    const updatedOpener = loadFromOpenerIfAvailable();
    if (updatedOpener && (updatedOpener.fileBlob || updatedOpener.file || updatedOpener.markdown)) {
      await applyComparePayload(updatedOpener);
    } else {
      const updatedDb = await loadCompareData();
      if (updatedDb) {
        await applyComparePayload(updatedDb);
      }
    }
  });
});
