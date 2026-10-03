// Bootstrap + selection flow: route → direction → stops → stop drill-down.
import * as api from './api.js';
import { renderRouteList, filterRoutes, routeStopsPanel, stopPanel, outlierPanel } from './panels.js';
import { initLayers, drawRoute, fitRoute, highlightGroup, flashStop } from './layers.js';

export const map = L.map('map', { preferCanvas: true }).fitBounds([
  [23.974, 120.498],
  [24.447, 121.322],
]);

L.tileLayer(window.RM_TILES, {
  maxZoom: 19,
  attribution: '&copy; OpenStreetMap contributors',
}).addTo(map);

initLayers(map);

export function status(msg) {
  document.getElementById('status-msg').textContent = msg ?? '';
}

function setChip(id, text) {
  const el = document.getElementById(id);
  if (el) el.querySelector('b').textContent = text;
}

let allRoutes = [];
let selectedKey = null;
let selectedRoute = null; // { route_id, dir }

function refreshList(q) {
  renderRouteList(filterRoutes(allRoutes, q), { selectedKey, onSelect: (r) => selectRoute(r) });
}

async function selectRouteById(routeId, dir = 0) {
  const r = allRoutes.find((x) => x.route_id === routeId);
  if (r) selectRoute(r, dir);
  else status(`route ${routeId} not in list`);
}

async function selectRoute(r, dir = 0) {
  selectedKey = `${r.route_id}`;
  selectedRoute = { route_id: r.route_id, dir };
  refreshList(document.getElementById('route-search').value);
  const { route_id } = selectedRoute;
  try {
    const [d, p, s] = await Promise.all([
      api.routeDetail(route_id),
      api.routePath(route_id, dir),
      api.routeStops(route_id, dir),
    ]);
    if (selectedRoute?.route_id !== route_id || selectedRoute.dir !== dir) return; // stale
    drawRoute(p, s, { onStopClick: (stop) => selectStop(stop.stop_version_id, stop) });
    fitRoute(p, s);
    routeStopsPanel(d, dir, s, {
      onDirection: (nd) => selectRoute(r, nd),
      onStopClick: (svid) => selectStop(svid),
    });
    if ((p.features ?? []).length === 0) status('no path data for this direction');
    location.hash = `#route=${route_id}&dir=${dir}`;
  } catch (e) {
    status(`route load failed: ${e.message}`);
  }
}

async function selectStop(stopVersionId) {
  try {
    const d = await api.stopDetail(stopVersionId);
    highlightGroup(d.group_members);
    stopPanel(d, { onPickRoute: (rid, dir) => selectRouteById(rid, dir ?? 0) });
    location.hash = `#stop=${stopVersionId}`;
  } catch (e) {
    status(`stop load failed: ${e.message}`);
  }
}

async function openOutliers() {
  try {
    const v = await api.outliers();
    outlierPanel(v.outliers ?? [], {
      onPick: (o) => {
        flashStop(o.lat, o.lon);
        map.setView([o.lat, o.lon], Math.max(map.getZoom(), 15));
      },
      onOpenRoute: (o) => selectRouteById(o.route_id, o.direction),
    });
    location.hash = '#outliers';
  } catch (e) {
    status(`outliers load failed: ${e.message}`);
  }
}

function parseHash() {
  const h = location.hash.replace(/^#/, '');
  const m = Object.fromEntries(
    h.split('&').filter(Boolean).map((kv) => {
      const i = kv.indexOf('=');
      return [kv.slice(0, i), kv.slice(i + 1)];
    })
  );
  return m;
}

async function boot() {
  const search = document.getElementById('route-search');
  search.addEventListener('input', () => refreshList(search.value));
  document.getElementById('chip-outliers').addEventListener('click', openOutliers);

  try {
    const [s, rv] = await Promise.all([api.stats(), api.routes()]);
    const c = s.counts ?? {};
    setChip('chip-routes', c.routes ?? '—');
    setChip('chip-stops', c.stops ?? '—');
    setChip('chip-stations', c.stations ?? '—');
    setChip('chip-segments', c.segments ?? '—');
    setChip('chip-match', `${s.match?.directions_with_stats ?? '—'}/${s.match?.directions_total ?? '—'}`);
    setChip('chip-outliers', s.outlier_stops ?? '—');
    if (s.batch) {
      status(`batch ${s.batch.batch_id} · captured ${s.batch.captured_at} · ${s.batch.status}`);
    }
    allRoutes = rv.routes ?? [];
    refreshList('');

    const h = parseHash();
    if (h.route) {
      selectRouteById(Number(h.route), Number(h.dir ?? 0));
    } else if (h.stop) {
      selectStop(Number(h.stop));
    }
  } catch (e) {
    status(`boot failed: ${e.message}`);
  }
}

boot();
