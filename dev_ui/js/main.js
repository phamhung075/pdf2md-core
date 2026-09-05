/**
 * Composition root (following pdf_awesome/js/main.js pattern).
 * Initializes UI modules and wires up dependencies.
 */
import { state } from './state.js';
import { initTabs } from './ui/tabs.js';
import { initPdfViewer, checkAndRenderTab } from './ui/pdf-viewer.js';
import { initStreamConsole } from './ui/stream-console.js';
import { initConversion } from './ui/conversion.js';

// Expose comparison payload for synchronous, zero-latency handover to /test/compare
window.getComparePayload = () => {
  return {
    fileName: state.selectedFile ? state.selectedFile.name : (state.currentPdfDoc ? 'document.pdf' : null),
    fileBlob: state.selectedFile || null,
    markdown: state.lastMarkdown || '',
    metadata: state.lastMetadata || null,
  };
};

initTabs((tabName) => {
  checkAndRenderTab(tabName);
});
initPdfViewer();
initStreamConsole();
initConversion();
