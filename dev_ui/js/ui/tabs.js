import { $, $$ } from '../infra/dom.js';

let onTabChangeCallback = null;

export function setTab(tabName) {
  const btn = $(`.tabs button[data-tab="${tabName}"]`);
  if (btn && btn.disabled) return;

  $$('.tabs button').forEach((x) => x.classList.toggle('active', x.dataset.tab === tabName));
  $$('.tab-pane').forEach((x) => x.classList.toggle('active', x.id === 'pane-' + tabName));

  if (onTabChangeCallback) {
    onTabChangeCallback(tabName);
  }
}

export function updateTabAvailability(isDocViewable, docType = 'document') {
  const tabPdf = $('tab-pdf');
  const tabSplit = $('tab-split');

  if (isDocViewable) {
    tabPdf.disabled = false;
    tabPdf.style.opacity = '1';
    tabPdf.style.cursor = 'pointer';
    tabPdf.title = `View original ${docType}`;

    tabSplit.disabled = false;
    tabSplit.style.opacity = '1';
    tabSplit.style.cursor = 'pointer';
    tabSplit.title = `Compare original ${docType} and Markdown side-by-side`;
  } else {
    tabPdf.disabled = true;
    tabPdf.style.opacity = '0.4';
    tabPdf.style.cursor = 'not-allowed';
    tabPdf.title = 'Available for PDF and image files only';

    tabSplit.disabled = true;
    tabSplit.style.opacity = '0.4';
    tabSplit.style.cursor = 'not-allowed';
    tabSplit.title = 'Available for PDF and image files only';

    const activeTab = $('.tabs button.active');
    if (activeTab && (activeTab.dataset.tab === 'pdf' || activeTab.dataset.tab === 'split')) {
      setTab('preview');
    }
  }
}

export function initTabs(onChange) {
  onTabChangeCallback = onChange;
  $$('.tabs button').forEach((b) => {
    b.addEventListener('click', () => setTab(b.dataset.tab));
  });
}
