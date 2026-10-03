// Bootstrap: map + top-bar stats chips. Sidebar/panels wire in later steps.
import * as api from './api.js';

export const map = L.map('map', { preferCanvas: true }).fitBounds([
  [23.974, 120.498],
  [24.447, 121.322],
]);

L.tileLayer(window.RM_TILES, {
  maxZoom: 19,
  attribution: '&copy; OpenStreetMap contributors',
}).addTo(map);

export function status(msg) {
  document.getElementById('status-msg').textContent = msg ?? '';
}

function setChip(id, text) {
  const el = document.getElementById(id);
  if (el) el.querySelector('b').textContent = text;
}

function fmtMatch(m) {
  if (!m) return '—';
  return `${m.directions_with_stats}/${m.directions_total}`;
}

async function boot() {
  try {
    const s = await api.stats();
    const c = s.counts ?? {};
    setChip('chip-routes', c.routes ?? '—');
    setChip('chip-stops', c.stops ?? '—');
    setChip('chip-stations', c.stations ?? '—');
    setChip('chip-segments', c.segments ?? '—');
    setChip('chip-match', fmtMatch(s.match));
    setChip('chip-outliers', s.outlier_stops ?? '—');
    if (s.batch) {
      status(`batch ${s.batch.batch_id} · captured ${s.batch.captured_at} · ${s.batch.status}`);
    }
  } catch (e) {
    status(`stats failed: ${e.message}`);
  }
}

boot();
