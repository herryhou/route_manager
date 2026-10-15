// Authored SVG icon set — one stroke, one weight (2px round on 24px grid),
// currentColor throughout. The craft floor bans unicode glyphs standing in
// for icons; everything visible imports from here (index.html inlines the
// same markup for its static chrome).

const svg = (inner) =>
  `<svg class="ic" viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${inner}</svg>`;

export const I = {
  back: svg('<path d="M15 18l-6-6 6-6"/>'),
  fwd: svg('<path d="M9 6l6 6-6 6"/>'),
  close: svg('<path d="M18 6L6 18M6 6l12 12"/>'),
  edit: svg('<path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z"/>'),
  refresh: svg('<path d="M21 12a9 9 0 1 1-2.64-6.36"/><path d="M21 3v6h-6"/>'),
  undo: svg('<polyline points="9 14 4 9 9 4"/><path d="M20 20v-7a4 4 0 0 0-4-4H4"/>'),
  warn: svg('<path d="M12 4L2.8 19.5h18.4z"/><path d="M12 10v4"/><path d="M12 16.8v.2"/>'),
  check: svg('<path d="M20 6L9 17l-5-5"/>'),
  panelL: svg('<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16"/>'),
  panelR: svg('<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M15 4v16"/>'),
  // Panel title glyphs — content-type marks for the detail-pane rims.
  route: svg('<circle cx="5" cy="19" r="2.5"/><circle cx="19" cy="5" r="2.5"/><path d="M6.8 17.2L17.2 6.8"/>'),
  piece: svg('<circle cx="5" cy="12" r="2.5"/><circle cx="19" cy="12" r="2.5"/><path d="M7.5 12h9"/>'),
  station: svg('<circle cx="12" cy="12" r="7"/>'),
  stop: svg('<rect x="8" y="3" width="8" height="11" rx="2"/><path d="M12 14v7"/>'),
  search: svg('<circle cx="11" cy="11" r="7"/><path d="M20 20l-4.35-4.35"/>'),
  fit: svg('<path d="M4 9V6a2 2 0 0 1 2-2h3"/><path d="M15 4h3a2 2 0 0 1 2 2v3"/><path d="M20 15v3a2 2 0 0 1-2 2h-3"/><path d="M9 20H6a2 2 0 0 1-2-2v-3"/>'),
  // Brand mark: the stop plate itself — enamel plate, two stops on the line.
  brand:
    '<div></div>',
};
