import { $, $$, escapeHtml } from '../infra/dom.js';
import { state } from '../state.js';

let streamConsole, streamEmpty, statusDot, streamStatusText;
let logCountBadge, autoscrollCb, levelFilterSel;

export function clearStreamConsole() {
  state.rawLogEntries = [];
  streamConsole.innerHTML = '';
  logCountBadge.textContent = '0';
  if (streamEmpty) {
    streamEmpty.style.display = 'block';
    streamConsole.appendChild(streamEmpty);
  }
}

export function setStreamStatus(statusKind, message) {
  statusDot.className = 'status-dot ' + (statusKind === 'active' ? 'active' : statusKind === 'error' ? 'error' : '');
  streamStatusText.textContent = message;
}

function applyFilterToRow(row, filter) {
  const lvl = row.dataset.level;
  if (filter === 'ALL') {
    row.classList.remove('hidden');
  } else if (filter === 'INFO') {
    row.classList.toggle('hidden', lvl === 'DEBUG');
  } else if (filter === 'WARN') {
    row.classList.toggle('hidden', lvl !== 'WARN' && lvl !== 'WARNING' && lvl !== 'ERROR' && lvl !== 'CRITICAL');
  }
}

export function appendStreamLog(entry) {
  if (streamEmpty && streamEmpty.parentNode) {
    streamEmpty.remove();
  }
  state.rawLogEntries.push(entry);
  logCountBadge.textContent = state.rawLogEntries.length;

  const row = document.createElement('div');
  row.className = 'log-row';
  row.dataset.level = entry.level || 'INFO';

  const timeStr = entry.ts ? new Date(entry.ts * 1000).toLocaleTimeString() : new Date().toLocaleTimeString();
  row.innerHTML = `
    <span class="log-time">[${timeStr}]</span>
    <span class="log-badge ${entry.level || 'INFO'}">${entry.level || 'INFO'}</span>
    <span class="log-origin">[${entry.logger || 'service'}]</span>
    <span class="log-msg">${escapeHtml(entry.message || '')}</span>
  `;

  applyFilterToRow(row, levelFilterSel.value);
  streamConsole.appendChild(row);

  if (autoscrollCb.checked) {
    streamConsole.scrollTop = streamConsole.scrollHeight;
  }
}

export function initStreamConsole() {
  streamConsole = $('stream-console');
  streamEmpty = $('stream-empty');
  statusDot = $('status-dot');
  streamStatusText = $('stream-status-text');
  logCountBadge = $('log-count');
  autoscrollCb = $('stream-autoscroll');
  levelFilterSel = $('stream-level-filter');

  levelFilterSel.addEventListener('change', () => {
    const filter = levelFilterSel.value;
    $$('.log-row').forEach((row) => applyFilterToRow(row, filter));
  });

  $('stream-clear').addEventListener('click', clearStreamConsole);

  $('stream-copy').addEventListener('click', () => {
    const copyBtn = $('stream-copy');
    const text = state.rawLogEntries.map((e) => {
      const timeStr = e.ts ? new Date(e.ts * 1000).toISOString() : '';
      return `${timeStr} [${e.level}] [${e.logger}]: ${e.message}`;
    }).join('\n');

    navigator.clipboard.writeText(text).then(() => {
      const originalText = copyBtn.textContent;
      copyBtn.textContent = 'Copied!';
      setTimeout(() => { copyBtn.textContent = originalText; }, 1500);
    });
  });
}
