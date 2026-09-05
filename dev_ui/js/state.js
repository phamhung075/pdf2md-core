/**
 * Centralized state object (following pdf_awesome pattern).
 */
export const state = {
  selectedFile: null,
  currentPdfDoc: null,
  currentPdfBytes: null,
  currentPdfUrl: null,
  lastMarkdown: '',
  lastMetadata: null,
  pdfScale: 'fit',
  splitScale: 'fit',
  currentPdfPage: 1,
  rawLogEntries: [],
  isConverting: false,
};

export function resetPdfState() {
  if (state.currentPdfUrl) {
    URL.revokeObjectURL(state.currentPdfUrl);
    state.currentPdfUrl = null;
  }
  state.currentPdfDoc = null;
  state.currentPdfBytes = null;
  state.currentPdfPage = 1;
}
