/**
 * Tiny shared DOM helpers (following pdf_awesome pattern).
 */
export const $ = (id) => document.getElementById(id);
export const $$ = (sel) => document.querySelectorAll(sel);

export function escapeHtml(str) {
  if (str == null) return '';
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#039;');
}

/**
 * Sanitizes HTML to prevent XSS attacks while preserving markdown formatting.
 * Removes script, iframe, object, embed, inline event handlers (on*), and javascript: URIs.
 */
export function sanitizeHtml(html) {
  if (!html) return '';
  const parser = new DOMParser();
  const doc = parser.parseFromString(html, 'text/html');

  const blockedTags = ['script', 'iframe', 'object', 'embed', 'style', 'link', 'meta', 'base', 'form'];
  blockedTags.forEach((tag) => {
    doc.querySelectorAll(tag).forEach((el) => el.remove());
  });

  const allElements = doc.body.querySelectorAll('*');
  allElements.forEach((el) => {
    const attrs = Array.from(el.attributes);
    for (const attr of attrs) {
      const name = attr.name.toLowerCase();
      const val = attr.value.trim().toLowerCase();

      if (name.startsWith('on')) {
        el.removeAttribute(attr.name);
      } else if (
        (name === 'href' || name === 'src' || name === 'action') &&
        (val.startsWith('javascript:') || val.startsWith('vbscript:') || val.startsWith('data:text/html'))
      ) {
        el.removeAttribute(attr.name);
      }
    }
  });

  return doc.body.innerHTML;
}
