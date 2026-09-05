import { $, escapeHtml, sanitizeHtml } from '../infra/dom.js';
import { state } from '../state.js';
import { setTab, updateTabAvailability } from './tabs.js';
import { isImageFile, isPdfFile, loadPdfDocument, updatePdfMetadata } from './pdf-viewer.js';
import { appendStreamLog, clearStreamConsole, setStreamStatus } from './stream-console.js';
import { saveCompareData } from '../infra/compare-sync.js';

let fileInput, runBtn, endpointSel, engineSel, enableStreamCb;
let logEl, metaEl, previewEl, splitPreviewEl, textEl, jsonEl, splitCopyMd;

function step(kind, text, ts) {
  const li = document.createElement('li');
  li.className = kind;
  li.textContent = `${ts ? ts.toLocaleTimeString() : ''} ${text}`.trim();
  logEl.appendChild(li);
  logEl.scrollTop = logEl.scrollHeight;
  return li;
}

function chip(label, value) {
  const c = document.createElement('span');
  c.className = 'chip';
  c.textContent = `${label} `;
  const b = document.createElement('b');
  b.textContent = String(value);
  c.appendChild(b);
  metaEl.appendChild(c);
}

function handleConversionSuccess(body, totalMs, file) {
  const ext = (file.name.split('.').pop() || '').toLowerCase();
  const routing = body.engine === 'docling-pdf'
    ? `.pdf → docling-pdf (Heron layout + TableFormer + RapidOCR PP-OCRv6 fr)`
    : body.engine === 'docling-image'
      ? `.${ext} → docling-image (Heron layout + TableFormer + RapidOCR PP-OCRv6 fr)`
      : body.engine === 'docling-native'
        ? `.${ext} → docling-native (file's own structure, no OCR)`
        : `engine: ${body.engine}`;

  step('ok', `Step 3 — routing: ${routing}`, new Date());
  step('ok', `Step 4 — converted: markdown ${body.markdown.length.toLocaleString()} chars · ` +
    `text ${(body.text || '').length.toLocaleString()} chars · numpages ${body.numpages}` +
    (body.duration_ms != null ? ` · server ${body.duration_ms} ms` : ''), new Date());
  step('ok', `Step 5 — checksum sha256 ${body.checksum.slice(0, 12)}…${body.checksum.slice(-6)}`, new Date());
  step('ok', `Step 6 — done in ${totalMs} ms total (upload + convert + download)`, new Date());

  chip('engine', body.engine);
  chip('numpages', body.numpages);
  chip('markdown', body.markdown.length.toLocaleString() + ' chars');
  chip('text', (body.text || '').length.toLocaleString() + ' chars');
  chip('server', (body.duration_ms ?? '—') + ' ms');
  chip('client', totalMs + ' ms');

  state.lastMarkdown = body.markdown || '';
  state.lastMetadata = body;
  state.selectedFile = file;
  try {
    const rawRendered = window.marked.parse(body.markdown || '_empty markdown_');
    const cleanRendered = sanitizeHtml(rawRendered);
    previewEl.innerHTML = cleanRendered;
    if (splitPreviewEl) splitPreviewEl.innerHTML = cleanRendered;
  } catch (e) {
    const errHtml = '<pre class="empty">renderer error: ' + escapeHtml(e.message) + '</pre>';
    previewEl.innerHTML = errHtml;
    if (splitPreviewEl) splitPreviewEl.innerHTML = errHtml;
  }

  textEl.textContent = body.text || '(no text field)';
  jsonEl.textContent = JSON.stringify(body, null, 2);

  updatePdfMetadata(body.numpages, file.size, file.name);

  saveCompareData({
    fileName: file.name,
    fileBlob: file,
    markdown: body.markdown || '',
    metadata: body,
  });
}

async function runConversion() {
  const file = fileInput.files[0];
  if (!file) return;

  runBtn.disabled = true;
  logEl.innerHTML = '';
  metaEl.innerHTML = '';
  clearStreamConsole();

  const isStreaming = enableStreamCb.checked;
  const queryParams = [];
  if (isStreaming) queryParams.push('stream=1');

  const engineVal = engineSel ? engineSel.value : 'auto';
  if (engineVal === 'docling') queryParams.push('engine=docling');
  if (engineVal === 'vision') queryParams.push('force_vision=1');

  const targetUrl = endpointSel.value + (queryParams.length ? '?' + queryParams.join('&') : '');

  if (isStreaming) {
    setTab('stream');
    setStreamStatus('active', 'Connecting & streaming live logs...');
  }

  const t0 = performance.now();
  step('info', `Step 1 — send: POST ${targetUrl} with ${file.size.toLocaleString()} raw bytes`, new Date());
  step('info', `         X-File-Name: ${encodeURIComponent(file.name)} (extension → pipeline routing)`);

  try {
    const res = await fetch(targetUrl, {
      method: 'POST',
      headers: {
        'content-type': 'application/octet-stream',
        'x-file-name': encodeURIComponent(file.name),
        ...(isStreaming ? { 'accept': 'text/event-stream' } : {}),
      },
      body: file,
      signal: AbortSignal.timeout(600000),
    });

    const totalMs = Math.round(performance.now() - t0);
    step(res.ok ? 'ok' : 'err', `Step 2 — HTTP ${res.status} ${res.statusText} established in ${totalMs} ms`, new Date());

    if (!res.ok && !isStreaming) {
      const body = await res.json().catch(() => res.text());
      const msg = typeof body === 'string' ? body : JSON.stringify(body);
      step('err', `Step 3 — failed: ${msg}`);
      jsonEl.textContent = typeof body === 'string' ? body : JSON.stringify(body, null, 2);
      setStreamStatus('error', 'Request failed');
      return;
    }

    if (isStreaming && res.headers.get('content-type')?.includes('text/event-stream')) {
      setStreamStatus('active', 'Converting document & streaming logs...');
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffer = '';
      let conversionResult = null;
      let streamError = null;

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        const parts = buffer.split('\n\n');
        buffer = parts.pop();

        for (const part of parts) {
          if (!part.trim()) continue;
          let eventType = 'message';
          let dataStr = '';
          for (const line of part.split('\n')) {
            if (line.startsWith('event:')) eventType = line.slice(6).trim();
            else if (line.startsWith('data:')) dataStr = line.slice(5).trim();
          }
          if (!dataStr) continue;

          let payload = {};
          try { payload = JSON.parse(dataStr); } catch { payload = { raw: dataStr }; }

          if (eventType === 'log') {
            appendStreamLog(payload);
          } else if (eventType === 'result') {
            conversionResult = payload;
          } else if (eventType === 'error') {
            streamError = payload.error || JSON.stringify(payload);
          }
        }
      }

      const fullMs = Math.round(performance.now() - t0);
      if (streamError) {
        step('err', `Step 3 — stream error: ${streamError}`);
        jsonEl.textContent = streamError;
        setStreamStatus('error', `Error: ${streamError}`);
      } else if (conversionResult) {
        setStreamStatus('idle', `Completed (${state.rawLogEntries.length} log events in ${fullMs} ms)`);
        handleConversionSuccess(conversionResult, fullMs, file);
      } else {
        step('warn', 'Stream finished without a result payload.');
        setStreamStatus('idle', 'Stream closed without result');
      }
    } else {
      const isJson = (res.headers.get('content-type') || '').includes('json');
      const body = isJson ? await res.json() : await res.text();
      if (!res.ok) {
        const msg = typeof body === 'string' ? body : JSON.stringify(body);
        step('err', `Step 3 — failed: ${msg}`);
        jsonEl.textContent = typeof body === 'string' ? body : JSON.stringify(body, null, 2);
        return;
      }
      handleConversionSuccess(body, totalMs, file);
    }
  } catch (err) {
    step('err', `Step 2 — request failed: ${err.message}`, new Date());
    setStreamStatus('error', err.message);
  } finally {
    runBtn.disabled = !fileInput.files.length;
  }
}

export function initConversion() {
  fileInput = $('file');
  runBtn = $('run');
  endpointSel = $('endpoint');
  engineSel = $('engine');
  enableStreamCb = $('enable-stream');
  logEl = $('log');
  metaEl = $('meta');
  previewEl = $('preview');
  splitPreviewEl = $('split-preview');
  textEl = $('text');
  jsonEl = $('json');
  splitCopyMd = $('split-copy-md');

  fileInput.addEventListener('change', async () => {
    const file = fileInput.files[0] || null;
    state.selectedFile = file;
    state.lastMarkdown = '';
    state.lastMetadata = null;
    runBtn.disabled = !file;
    logEl.innerHTML = '';
    metaEl.innerHTML = '';
    previewEl.innerHTML = '<span class="empty">No conversion yet.</span>';
    textEl.textContent = '—';
    jsonEl.textContent = '—';
    if (splitPreviewEl) splitPreviewEl.innerHTML = '<span class="empty">No conversion yet.</span>';

    const isPdf = isPdfFile(file);
    const isImg = isImageFile(file);
    const isViewable = isPdf || isImg;
    const docType = isImg ? 'Image' : (isPdf ? 'PDF' : 'document');
    const badge = $('split-filename-badge');
    if (badge) badge.textContent = file ? file.name : '—';
    updateTabAvailability(isViewable, docType);
    await loadPdfDocument(file);

    if (file) {
      step('info', `File selected: ${file.name} (${file.size.toLocaleString()} bytes)`);
    } else {
      step('info', 'Waiting for a file…');
    }
  });

  runBtn.addEventListener('click', runConversion);

  splitCopyMd.addEventListener('click', () => {
    if (!state.lastMarkdown) return;
    navigator.clipboard.writeText(state.lastMarkdown).then(() => {
      const orig = splitCopyMd.textContent;
      splitCopyMd.textContent = 'Copied!';
      setTimeout(() => { splitCopyMd.textContent = orig; }, 1500);
    });
  });
}
