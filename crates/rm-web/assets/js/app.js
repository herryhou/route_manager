// Bootstrap: map + stats chips + route sidebar. Detail interactions wire in later steps.
import * as api from './api.js';
import { renderRouteList, filterRoutes, showDetail, kv } from './panels.js';

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

let allRoutes = [];
let selectedKey = null;

function refreshList(q) {
  renderRouteList(filterRoutes(allRoutes, q), {
    selectedKey,
    onSelect: (r) => selectRoute(r),
  });
}

async function selectRoute(r) {
  selectedKey = `${r.route_id}`;
  refreshList(document.getElementById('route-search').value);
  try {
    const d = await api.routeDetail(r.route_id);
    showDetail(`
      <h2>Route ${d.route_code}</h2>
      ${kv([
        ['name', d.subroute_name],
        ['providers', d.providers.join(', ')],
        ['version', `v${d.version_no} · ${d.status}`],
        ['valid from', d.valid_from],
        ['side routes', d.side_routes.join(', ')],
        ['main routes', d.main_routes.join(', ')],
      ])}
      <p class="muted">map drawing lands in the next step</p>
    `);
  } catch (e) {
    status(`route detail failed: ${e.message}`);
  }
}

async function boot() {
  const search = document.getElementById('route-search');
  search.addEventListener('input', () => refreshList(search.value));

  try {
    const [s, rv] = await Promise.all([api.stats(), api.routes()]);
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
    allRoutes = rv.routes ?? [];
    refreshList('');
  } catch (e) {
    status(`boot failed: ${e.message}`);
  }
}

boot();
