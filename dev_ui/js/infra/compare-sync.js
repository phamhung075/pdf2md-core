/**
 * Synchronization layer between test page and dedicated comparison page.
 * Uses IndexedDB for large binary payloads (PDF ArrayBuffer) and BroadcastChannel for real-time events.
 */

const DB_NAME = 'markdown_extract_compare_db';
const DB_VERSION = 1;
const STORE_NAME = 'compare_store';
const KEY = 'active_comparison';

let channel = null;
try {
  channel = new BroadcastChannel('markdown_extract_compare_channel');
} catch (e) {
  console.warn('BroadcastChannel not supported:', e);
}

function openDb() {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB_NAME, DB_VERSION);
    req.onupgradeneeded = (e) => {
      const db = e.target.result;
      if (!db.objectStoreNames.contains(STORE_NAME)) {
        db.createObjectStore(STORE_NAME);
      }
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

/**
 * Saves comparison payload to IndexedDB and broadcasts notification.
 */
export async function saveCompareData(payload) {
  try {
    if (!payload) return;

    const cleanPayload = {
      fileName: payload.fileName || 'document.pdf',
      markdown: payload.markdown || '',
      metadata: payload.metadata || null,
      updatedAt: Date.now(),
    };

    // Store Blob safely (File is also a Blob). Avoid storing detached ArrayBuffer.
    if (payload.fileBlob instanceof Blob) {
      cleanPayload.fileBlob = payload.fileBlob;
    } else if (payload.pdfBytes) {
      try {
        if (payload.pdfBytes instanceof ArrayBuffer && payload.pdfBytes.byteLength > 0) {
          cleanPayload.fileBlob = new Blob([payload.pdfBytes], { type: 'application/pdf' });
        } else if (ArrayBuffer.isView(payload.pdfBytes) && payload.pdfBytes.byteLength > 0) {
          cleanPayload.fileBlob = new Blob([payload.pdfBytes.buffer], { type: 'application/pdf' });
        }
      } catch (e) {
        console.warn('Could not convert pdfBytes to Blob for IndexedDB:', e);
      }
    }

    const db = await openDb();
    await new Promise((resolve, reject) => {
      const tx = db.transaction(STORE_NAME, 'readwrite');
      const store = tx.objectStore(STORE_NAME);
      const req = store.put(cleanPayload, KEY);
      req.onsuccess = () => resolve();
      req.onerror = () => reject(req.error);
    });

    if (channel) {
      channel.postMessage({
        type: 'compare_data_updated',
        fileName: cleanPayload.fileName,
        hasPdf: !!cleanPayload.fileBlob,
        hasMarkdown: !!cleanPayload.markdown,
        updatedAt: cleanPayload.updatedAt,
      });
    }
  } catch (err) {
    console.error('Failed to save compare data to IndexedDB:', err);
  }
}

/**
 * Loads the active comparison payload from IndexedDB.
 */
export async function loadCompareData() {
  try {
    const db = await openDb();
    return await new Promise((resolve, reject) => {
      const tx = db.transaction(STORE_NAME, 'readonly');
      const store = tx.objectStore(STORE_NAME);
      const req = store.get(KEY);
      req.onsuccess = () => resolve(req.result || null);
      req.onerror = () => reject(req.error);
    });
  } catch (err) {
    console.error('Failed to load compare data from IndexedDB:', err);
    return null;
  }
}

/**
 * Listens for updates from other tabs.
 */
export function onCompareDataUpdated(callback) {
  if (!channel) return () => {};
  const handler = (e) => {
    if (e.data && e.data.type === 'compare_data_updated') {
      callback(e.data);
    }
  };
  channel.addEventListener('message', handler);
  return () => channel.removeEventListener('message', handler);
}
