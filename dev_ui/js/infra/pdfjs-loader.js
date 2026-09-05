/**
 * Lazy-loads and configures pdf.min.js and pdf.worker.min.js.
 */
let pdfjsLoaded = false;

function appendScript(src) {
  return new Promise((resolve, reject) => {
    const existing = document.querySelector(`script[src="${src}"]`);
    if (existing) {
      resolve();
      return;
    }
    const script = document.createElement("script");
    script.src = src;
    script.onload = resolve;
    script.onerror = () => reject(new Error(`Failed to load script (${src})`));
    document.head.appendChild(script);
  });
}

export async function loadPdfJs() {
  if (pdfjsLoaded && window.pdfjsLib) {
    return window.pdfjsLib;
  }

  if (!window.pdfjsLib) {
    await appendScript("/test/pdf.min.js");
  }

  if (window.pdfjsLib) {
    const workerUrl = new URL("/test/pdf.worker.min.js", window.location.href).href;
    window.pdfjsLib.GlobalWorkerOptions.workerSrc = workerUrl;
    pdfjsLoaded = true;
    return window.pdfjsLib;
  }

  throw new Error("Failed to initialize PDF.js library");
}
