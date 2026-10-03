// Bootstrap + selection flow: route → direction → stops → stop drill-down.
import * as api from './api.js';
import { renderRouteList, filterRoutes, routeStopsPanel, stopPanel, outlierPanel, segmentPanel } from './panels.js';
import { initLayers, drawRoute, fitRoute, highlightGroup, flashStop, drawSegments, clearSegments, drawWays, clearWays, drawRaw, clearRaw, drawStopsLayer, clearStopsLayer } from './layers.js';

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
    if (document.getElementById('layer-osm').checked) loadWays(route_id, dir);
    else clearWays();
    if (document.getElementById('layer-raw').checked) loadRaw(route_id, dir);
    else clearRaw();
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

async function loadWays(routeId, dir) {
  try {
    const fc = await api.routeWays(routeId, dir);
    if (selectedRoute?.route_id !== routeId || selectedRoute.dir !== dir) return; // stale
    drawWays(fc);
  } catch (e) {
    status(`ways load failed: ${e.message}`);
  }
}

async function loadRaw(routeId, dir) {
  try {
    const v = await api.routeRaw(routeId, dir);
    if (selectedRoute?.route_id !== routeId || selectedRoute.dir !== dir) return; // stale
    drawRaw(v);
  } catch (e) {
    status(`raw load failed: ${e.message}`);
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

/// Viewport segments layer: checkbox + zoom ≥13 gated, debounced on moveend.
/// Viewport stops/stations layer: checkbox + zoom gated
/// (<12 off, 12–14 stations, ≥15 stops), debounced on moveend.
let stopsTimer = null;
async function refreshStopsLayer() {
  if (!document.getElementById('layer-stops').checked) return;
  const z = map.getZoom();
  if (z < 12) {
    clearStopsLayer();
    return;
  }
  const kind = z >= 15 ? 'stop' : 'station';
  const b = map.getBounds();
  const bbox = [b.getWest(), b.getSouth(), b.getEast(), b.getNorth()]
    .map((x) => x.toFixed(4))
    .join(',');
  try {
    const v = await api.stopsBbox(bbox, kind);
    drawStopsLayer(v, { onStopClick: (svid) => selectStop(svid) });
    if (v.truncated) status(`${kind} layer truncated — zoom in`);
  } catch (e) {
    status(`stops layer load failed: ${e.message}`);
  }
}

let segmentsTimer = null;
async function refreshSegments() {
  if (!document.getElementById('layer-segments').checked) return;
  const b = map.getBounds();
  if (map.getZoom() < 13) {
    clearSegments();
    status('segments need zoom ≥13');
    return;
  }
  const pad = 0.05;
  const bbox = [
    b.getWest() - pad, b.getSouth() - pad, b.getEast() + pad, b.getNorth() + pad,
  ].map((x) => x.toFixed(4)).join(',');
  try {
    const v = await api.segmentsBbox(bbox);
    drawSegments(v, { onSegmentClick: selectSegment });
    if (v.truncated) status('segments truncated — zoom in');
  } catch (e) {
    status(`segments load failed: ${e.message}`);
  }
}

async function selectSegment(segmentId) {
  try {
    const d = await api.segmentDetail(segmentId);
    segmentPanel(d, { onPickRoute: (rid, dir) => selectRouteById(rid, dir ?? 0) });
    location.hash = `#segment=${segmentId}`;
  } catch (e) {
    status(`segment load failed: ${e.message}`);
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
  const segToggle = document.getElementById('layer-segments');
  segToggle.addEventListener('change', () => {
    if (segToggle.checked) refreshSegments();
    else clearSegments();
  });
  document.getElementById('layer-osm').addEventListener('change', (e) => {
    if (e.target.checked && selectedRoute) loadWays(selectedRoute.route_id, selectedRoute.dir);
    else clearWays();
  });
  document.getElementById('layer-raw').addEventListener('change', (e) => {
    if (e.target.checked && selectedRoute) loadRaw(selectedRoute.route_id, selectedRoute.dir);
    else clearRaw();
  });
  const stopsToggle = document.getElementById('layer-stops');
  stopsToggle.addEventListener('change', () => {
    if (stopsToggle.checked) refreshStopsLayer();
    else clearStopsLayer();
  });
  map.on('moveend', () => {
    clearTimeout(segmentsTimer);
    clearTimeout(stopsTimer);
    segmentsTimer = setTimeout(refreshSegments, 250);
    stopsTimer = setTimeout(refreshStopsLayer, 250);
  });

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
    } else if (h.segment) {
      selectSegment(h.segment);
    }
  } catch (e) {
    status(`boot failed: ${e.message}`);
  }
}

boot();
