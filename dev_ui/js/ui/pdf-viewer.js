import { $, escapeHtml } from '../infra/dom.js';
import { loadPdfJs } from '../infra/pdfjs-loader.js';
import { state, resetPdfState } from '../state.js';
import { saveCompareData } from '../infra/compare-sync.js';

let pdfPagesList, splitPdfPagesList, pdfViewport, splitPdfViewport;
let pdfFileInfo, pdfOpenBtn, pdfDownloadBtn, splitPdfOpen;
let pdfEmpty, pdfContent, splitEmpty, splitContent;
let isRenderingMain = false;
let isRenderingSplit = false;

const IMAGE_EXTENSIONS = ['.jpg', '.jpeg', '.png', '.webp', '.bmp', '.tiff', '.tif'];

export function isImageFile(file) {
  if (!file) return false;
  if (file.type && file.type.startsWith('image/')) return true;
  const name = (file.name || '').toLowerCase();
  return IMAGE_EXTENSIONS.some((ext) => name.endsWith(ext));
}

export function isPdfFile(file) {
  if (!file) return false;
  return file.type === 'application/pdf' || (file.name || '').toLowerCase().endsWith('.pdf');
}

export function renderImagePage(containerEl, isSplit) {
  containerEl.innerHTML = '';
  const pageWrapper = document.createElement('div');
  pageWrapper.className = 'pdf-page-card';
  pageWrapper.style.display = 'flex';
  pageWrapper.style.justifyContent = 'center';
  pageWrapper.style.alignItems = 'center';
  pageWrapper.style.padding = '16px';

  const img = document.createElement('img');
  img.src = state.currentPdfUrl;
  img.alt = state.selectedFile ? state.selectedFile.name : 'Document image';
  img.style.maxWidth = '100%';
  img.style.maxHeight = isSplit ? '80vh' : '85vh';
  img.style.objectFit = 'contain';
  img.style.borderRadius = '4px';
  img.style.boxShadow = '0 2px 10px rgba(0,0,0,0.12)';

  pageWrapper.appendChild(img);
  containerEl.appendChild(pageWrapper);
}

export async function renderPdfPages(containerEl, targetScale, isSplit) {
  if (!state.currentPdfDoc) return;
  if (isSplit ? isRenderingSplit : isRenderingMain) return;
  if (isSplit) isRenderingSplit = true; else isRenderingMain = true;

  try {
    containerEl.innerHTML = '';
    const totalPages = state.currentPdfDoc.numPages;
    const containerWidth = containerEl.clientWidth || (isSplit ? 450 : 750);
    const availableWidth = Math.max(240, containerWidth - 36);

    for (let p = 1; p <= totalPages; p++) {
      const page = await state.currentPdfDoc.getPage(p);
      const unscaledViewport = page.getViewport({ scale: 1 });

      let scale;
      if (targetScale === 'fit') {
        scale = availableWidth / unscaledViewport.width;
      } else {
        scale = targetScale;
      }

      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const viewport = page.getViewport({ scale: scale * dpr });

      const pageWrapper = document.createElement('div');
      pageWrapper.className = 'pdf-page-card';
      pageWrapper.id = (isSplit ? 'split-page-' : 'main-page-') + p;
      pageWrapper.dataset.page = p;

      if (totalPages > 1) {
        const pageNum = document.createElement('div');
        pageNum.className = 'pdf-page-number';
        pageNum.textContent = `Page ${p} of ${totalPages}`;
        pageWrapper.appendChild(pageNum);
      }

      const canvas = document.createElement('canvas');
      canvas.className = 'pdf-page-canvas';
      canvas.width = viewport.width;
      canvas.height = viewport.height;
      canvas.style.width = `${Math.round(viewport.width / dpr)}px`;
      canvas.style.height = `${Math.round(viewport.height / dpr)}px`;

      const ctx = canvas.getContext('2d');
      await page.render({ canvasContext: ctx, viewport }).promise;

      pageWrapper.appendChild(canvas);
      containerEl.appendChild(pageWrapper);
    }

    if (isSplit) {
      $('split-page-indicator').textContent = `1 / ${totalPages}`;
    } else {
      $('pdf-page-display').textContent = `1 / ${totalPages}`;
    }
  } catch (err) {
    console.error('PDF render error:', err);
    containerEl.innerHTML = `<div class="empty" style="color:var(--err);padding:20px;">Error rendering PDF: ${escapeHtml(err.message)}</div>`;
  } finally {
    if (isSplit) isRenderingSplit = false; else isRenderingMain = false;
  }
}

export async function loadPdfDocument(file) {
  resetPdfState();
  state.selectedFile = file || null;
  if (!file) {
    clearPdfUI(null);
    return;
  }

  const isPdf = isPdfFile(file);
  const isImg = isImageFile(file);
  if (!isPdf && !isImg) {
    clearPdfUI(file);
    return;
  }

  state.currentPdfUrl = URL.createObjectURL(file);
  pdfOpenBtn.href = state.currentPdfUrl;
  pdfDownloadBtn.href = state.currentPdfUrl;
  pdfDownloadBtn.download = file.name;
  splitPdfOpen.href = state.currentPdfUrl;

  const sizeKb = (file.size / 1024).toFixed(1);
  const docBadge = $('pdf-doc-badge');

  pdfEmpty.style.display = 'none';
  pdfContent.style.display = 'block';
  splitEmpty.style.display = 'none';
  splitContent.style.display = 'block';

  const filenameBadge = $('split-filename-badge');
  if (filenameBadge) filenameBadge.textContent = file.name;

  if (isImg) {
    if (docBadge) {
      docBadge.textContent = 'IMAGE';
      docBadge.style.borderColor = '#10b981';
      docBadge.style.color = '#10b981';
    }
    pdfFileInfo.textContent = `${file.name} · 1 image (${sizeKb} KB)`;
    $('pdf-page-display').textContent = '1 / 1';
    $('split-page-indicator').textContent = '1 / 1';

    saveCompareData({
      fileName: file.name,
      fileBlob: file,
      markdown: state.lastMarkdown || '',
      metadata: { numpages: 1, isImage: true },
    });

    renderImagePage(pdfPagesList, false);
    renderImagePage(splitPdfPagesList, true);
    return;
  }

  if (docBadge) {
    docBadge.textContent = 'PDF';
    docBadge.style.borderColor = '#0284c7';
    docBadge.style.color = 'var(--accent)';
  }

  try {
    pdfPagesList.innerHTML = '<div class="empty" style="padding:20px;">Loading and rendering PDF pages...</div>';
    splitPdfPagesList.innerHTML = '<div class="empty" style="padding:20px;">Loading and rendering PDF pages...</div>';

    const pdfjs = await loadPdfJs();
    state.currentPdfBytes = await file.arrayBuffer();
    state.currentPdfDoc = await pdfjs.getDocument({ data: new Uint8Array(state.currentPdfBytes) }).promise;

    const totalPages = state.currentPdfDoc.numPages;
    pdfFileInfo.textContent = `${file.name} · ${totalPages} page${totalPages === 1 ? '' : 's'} (${sizeKb} KB)`;
    $('pdf-page-display').textContent = `1 / ${totalPages}`;
    $('split-page-indicator').textContent = `1 / ${totalPages}`;

    saveCompareData({
      fileName: file.name,
      fileBlob: file,
      markdown: state.lastMarkdown || '',
      metadata: { numpages: totalPages },
    });

    await renderPdfPages(pdfPagesList, state.pdfScale, false);
    await renderPdfPages(splitPdfPagesList, state.splitScale, true);
  } catch (err) {
    console.error('Failed to load PDF:', err);
    pdfPagesList.innerHTML = `<div class="empty" style="color:var(--err);padding:20px;">Failed to load PDF: ${escapeHtml(err.message)}</div>`;
    splitPdfPagesList.innerHTML = `<div class="empty" style="color:var(--err);padding:20px;">Failed to load PDF: ${escapeHtml(err.message)}</div>`;
  }
}

function clearPdfUI(file) {
  pdfOpenBtn.removeAttribute('href');
  pdfDownloadBtn.removeAttribute('href');
  splitPdfOpen.removeAttribute('href');
  pdfPagesList.innerHTML = '';
  splitPdfPagesList.innerHTML = '';

  pdfContent.style.display = 'none';
  pdfEmpty.style.display = 'flex';
  pdfEmpty.textContent = file
    ? `Selected file (${file.name.split('.').pop() || 'unknown'}) is not a PDF or image. Document preview is available for .pdf, .jpg, .png, .webp, etc.`
    : 'No document selected. Select a .pdf or image file above to preview the original document.';

  splitContent.style.display = 'none';
  splitEmpty.style.display = 'flex';
  splitEmpty.textContent = file
    ? 'Side-by-side view is available for .pdf and image files.'
    : 'No document selected. Select a .pdf or image file and convert to compare side-by-side.';
}

export function updatePdfMetadata(numPages, fileSize, fileName) {
  if (state.currentPdfDoc || (fileName && (fileName.toLowerCase().endsWith('.pdf') || isImageFile({ name: fileName })))) {
    const pagesInfo = numPages != null ? ` · ${numPages} page${numPages === 1 ? '' : 's'}` : '';
    pdfFileInfo.textContent = `${fileName}${pagesInfo} (${(fileSize / 1024).toFixed(1)} KB)`;
  }
}

export function checkAndRenderTab(tabName) {
  if (isImageFile(state.selectedFile)) {
    if (tabName === 'pdf' && !pdfPagesList.querySelector('img')) {
      renderImagePage(pdfPagesList, false);
    } else if (tabName === 'split' && !splitPdfPagesList.querySelector('img')) {
      renderImagePage(splitPdfPagesList, true);
    }
    return;
  }
  if (!state.currentPdfDoc) return;
  if (tabName === 'pdf' && !pdfPagesList.querySelector('canvas')) {
    renderPdfPages(pdfPagesList, state.pdfScale, false);
  } else if (tabName === 'split' && !splitPdfPagesList.querySelector('canvas')) {
    renderPdfPages(splitPdfPagesList, state.splitScale, true);
  }
}

export function initPdfViewer() {
  pdfPagesList = $('pdf-pages-list');
  splitPdfPagesList = $('split-pdf-pages-list');
  pdfViewport = $('pdf-viewport');
  splitPdfViewport = $('split-pdf-viewport');
  pdfFileInfo = $('pdf-file-info');
  pdfOpenBtn = $('pdf-open-btn');
  pdfDownloadBtn = $('pdf-download-btn');
  splitPdfOpen = $('split-pdf-open');
  pdfEmpty = $('pdf-empty');
  pdfContent = $('pdf-content');
  splitEmpty = $('split-empty');
  splitContent = $('split-content');

  // Zoom controls
  $('pdf-zoom-in').addEventListener('click', () => {
    let cur = (typeof state.pdfScale === 'number') ? state.pdfScale : 1.0;
    state.pdfScale = Math.min(3.0, Math.round((cur + 0.25) * 100) / 100);
    $('pdf-zoom-reset').textContent = `${Math.round(state.pdfScale * 100)}%`;
    renderPdfPages(pdfPagesList, state.pdfScale, false);
  });

  $('pdf-zoom-out').addEventListener('click', () => {
    let cur = (typeof state.pdfScale === 'number') ? state.pdfScale : 1.0;
    state.pdfScale = Math.max(0.4, Math.round((cur - 0.25) * 100) / 100);
    $('pdf-zoom-reset').textContent = `${Math.round(state.pdfScale * 100)}%`;
    renderPdfPages(pdfPagesList, state.pdfScale, false);
  });

  $('pdf-zoom-reset').addEventListener('click', () => {
    state.pdfScale = 'fit';
    $('pdf-zoom-reset').textContent = 'Fit';
    renderPdfPages(pdfPagesList, 'fit', false);
  });

  $('split-zoom-in').addEventListener('click', () => {
    let cur = (typeof state.splitScale === 'number') ? state.splitScale : 0.8;
    state.splitScale = Math.min(2.5, Math.round((cur + 0.2) * 100) / 100);
    renderPdfPages(splitPdfPagesList, state.splitScale, true);
  });

  $('split-zoom-out').addEventListener('click', () => {
    let cur = (typeof state.splitScale === 'number') ? state.splitScale : 0.8;
    state.splitScale = Math.max(0.3, Math.round((cur - 0.2) * 100) / 100);
    renderPdfPages(splitPdfPagesList, state.splitScale, true);
  });

  // Navigation
  $('pdf-prev-page').addEventListener('click', () => {
    if (!state.currentPdfDoc || state.currentPdfPage <= 1) return;
    state.currentPdfPage--;
    const el = document.getElementById('main-page-' + state.currentPdfPage);
    if (el) el.scrollIntoView({ behavior: 'smooth', block: 'start' });
    $('pdf-page-display').textContent = `${state.currentPdfPage} / ${state.currentPdfDoc.numPages}`;
  });

  $('pdf-next-page').addEventListener('click', () => {
    if (!state.currentPdfDoc || state.currentPdfPage >= state.currentPdfDoc.numPages) return;
    state.currentPdfPage++;
    const el = document.getElementById('main-page-' + state.currentPdfPage);
    if (el) el.scrollIntoView({ behavior: 'smooth', block: 'start' });
    $('pdf-page-display').textContent = `${state.currentPdfPage} / ${state.currentPdfDoc.numPages}`;
  });

  // Scroll tracking
  pdfViewport.addEventListener('scroll', () => {
    if (!state.currentPdfDoc) return;
    const cards = pdfPagesList.querySelectorAll('.pdf-page-card');
    const scrollMiddle = pdfViewport.scrollTop + 80;
    for (const card of cards) {
      if (card.offsetTop <= scrollMiddle && (card.offsetTop + card.offsetHeight) > scrollMiddle) {
        state.currentPdfPage = parseInt(card.dataset.page, 10);
        $('pdf-page-display').textContent = `${state.currentPdfPage} / ${state.currentPdfDoc.numPages}`;
        break;
      }
    }
  }, { passive: true });

  splitPdfViewport.addEventListener('scroll', () => {
    if (!state.currentPdfDoc) return;
    const cards = splitPdfPagesList.querySelectorAll('.pdf-page-card');
    const scrollMiddle = splitPdfViewport.scrollTop + 80;
    for (const card of cards) {
      if (card.offsetTop <= scrollMiddle && (card.offsetTop + card.offsetHeight) > scrollMiddle) {
        $('split-page-indicator').textContent = `${card.dataset.page} / ${state.currentPdfDoc.numPages}`;
        break;
      }
    }
  }, { passive: true });

  const maxBtn = $('split-btn-maximize');
  if (maxBtn) {
    maxBtn.addEventListener('click', () => {
      const pane = $('pane-split');
      const isMax = pane.classList.toggle('maximized');
      maxBtn.textContent = isMax ? '✕ Exit Max' : '⛶ Maximize';
      renderPdfPages(splitPdfPagesList, state.splitScale, true);
    });
  }

  const openPageBtn = $('split-btn-open-page');
  if (openPageBtn) {
    openPageBtn.addEventListener('click', () => {
      const file = state.selectedFile;
      if (file || state.lastMarkdown) {
        saveCompareData({
          fileName: file ? file.name : 'document.pdf',
          fileBlob: file,
          markdown: state.lastMarkdown || '',
          metadata: state.lastMetadata || null,
        });
      }
    });
  }
}
