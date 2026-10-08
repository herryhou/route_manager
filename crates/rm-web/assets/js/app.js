// Bootstrap + selection flow: route → direction → stops → stop drill-down.
import * as api from './api.js';
import { renderRouteList, renderStationList, filterRoutes, routeStopsPanel, stopPanel, stationPanel, stopPopupEl, stationPopupEl, outlierPanel, matchDevPanel, variantPanel, segmentPanel, pathPiecePanel, setPaneChangeHook, pushPaneHistory, popPaneHistory, popPaneForward, closePane, setEditable } from './panels.js';
import { initLayers, layers, clearHighlight, drawRoute, fitRoute, highlightGroup, flashStop, drawSegments, clearSegments, drawRaw, clearRaw, drawStopsLayer, clearStopsLayer, setBearingsOn, drawSegmentHighlight, clearSegmentHighlight } from './layers.js';

export const map = L.map('map', { preferCanvas: true }).fitBounds([
  [23.974, 120.498],
  [24.447, 121.322],
]);

L.tileLayer(window.RM_TILES, {
  maxZoom: 23,
  attribution: '&copy; OpenStreetMap contributors',
}).addTo(map);

initLayers(map);

export function status(msg) {
  document.getElementById('status-msg').textContent = msg ?? '';
}

/// Spinner in the statusbar — on while a primary fetch runs (route/segment/
/// station/QA panel loads), off in finally. Background layers don't touch it.
function busy(on) {
  document.getElementById('busy').hidden = !on;
}

function setChip(id, text) {
  const el = document.getElementById(id);
  if (!el) return;
  el.querySelector('b').textContent = text;
  // QA chips: non-zero count reads red — issues visible at a glance.
  if (el.classList.contains('chip-click')) el.classList.toggle('hot', parseInt(text, 10) > 0);
}

let allRoutes = [];
let selectedKey = null;
let selectedRoute = null; // { route_id, dir }
let lastRouteDraw = null; // { p, s, onStopClick } — for bearing-toggle redraw
let editMode = false;
let pendingPlace = null; // armed move-place map handler — replaced/cancelled, never stacked

function refreshList(q) {
  renderRouteList(filterRoutes(allRoutes, q), { selectedKey, onSelect: (r) => selectRoute(r) });
}

/// Debounced station-name search feeding the sidebar station list; seq guards stale responses.
let stationTimer = null;
let stationSeq = 0;
function refreshStations(q) {
  clearTimeout(stationTimer);
  q = q.trim();
  if (!q) {
    renderStationList([]);
    return;
  }
  const seq = ++stationSeq;
  stationTimer = setTimeout(async () => {
    try {
      const v = await api.stationsSearch(q);
      if (seq !== stationSeq) return; // stale
      renderStationList(v.stations ?? [], { onSelect: (s) => selectStation(s.station_id) });
    } catch (e) {
      status(`station search failed: ${e.message}`);
    }
  }, 300);
}

/// Pane history (semantic back/forward): each pane's current content is
/// { key, reopen }; different content taking the pane pushes the old one
/// onto that pane's back stack. Back/forward pop and re-run the closure —
/// nulling current first suppresses the push inside the re-run.
const paneCurrent = { context: null, point: null };

function paneOpen(kind, key, reopen) {
  if (paneCurrent[kind] && paneCurrent[kind].key !== key) {
    pushPaneHistory(kind, paneCurrent[kind].reopen);
  }
  paneCurrent[kind] = { key, reopen };
}

function paneBack(kind) {
  const cur = paneCurrent[kind];
  const reopen = popPaneHistory(kind, cur?.reopen);
  if (!reopen) return;
  paneCurrent[kind] = null;
  reopen();
}

function paneForward(kind) {
  const cur = paneCurrent[kind];
  const reopen = popPaneForward(kind, cur?.reopen);
  if (!reopen) return;
  paneCurrent[kind] = null;
  reopen();
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
  busy(true);
  try {
    const geom = document.getElementById('route-geom').value;
    const [d, p, s] = await Promise.all([
      api.routeDetail(route_id),
      geom === 'osm' ? api.routeWays(route_id, dir) : api.routePath(route_id, dir),
      api.routeStops(route_id, dir),
    ]);
    if (selectedRoute?.route_id !== route_id || selectedRoute.dir !== dir) return; // stale
    const onStopClick = (stop, marker) => openStopPopup(stop.stop_version_id, marker);
    // Path click: point pane + red highlight of that piece / way run, same
    // selectSegment pattern (pane change hook clears it). The reopen closure
    // replays it from the captured properties — no refetch.
    const onPathClick = (feature) => {
      const p = feature.properties;
      const show = () => {
        pathPiecePanel(p, { onPickSegment: selectSegment });
        if (feature.geometry) {
          drawSegmentHighlight(feature.geometry, p.osm_way_id != null ? 'selected way' : 'selected piece');
        }
      };
      paneOpen('point', `piece:${route_id}:${dir}:${p.ordinal}:${p.osm_way_id ?? 'draft'}`, show);
      show();
    };
    lastRouteDraw = { p, s, onStopClick, onPathClick };
    drawRoute(p, s, { onStopClick, onPathClick });
    if (document.getElementById('layer-raw').checked) loadRaw(route_id, dir);
    else clearRaw();
    fitRoute(p, s);
    paneOpen('context', `route:${route_id}:${dir}`, () => selectRouteById(route_id, dir));
    routeStopsPanel(d, dir, s, {
      onDirection: (nd) => selectRoute(r, nd),
      onStopClick: (svid) => popupStop(svid),
    });
    if ((p.features ?? []).length === 0) status('no path data for this direction');
    location.hash = `#route=${route_id}&dir=${dir}`;
  } catch (e) {
    status(`route load failed: ${e.message}`);
  } finally {
    busy(false);
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

/// Compact popup on the clicked marker; point pane mirrors the detail.
async function openStopPopup(stopVersionId, marker) {
  try {
    const d = await api.stopDetail(stopVersionId);
    const onPickRoute = (rid, dir) => selectRouteById(rid, dir ?? 0);
    paneOpen('point', `stop:${stopVersionId}`, () => selectStop(stopVersionId));
    const el = stopPopupEl(d, { onPickRoute });
    marker.unbindPopup();
    marker.bindPopup(el, { maxWidth: 300 }).openPopup();
    stopPanel(d, { onPickRoute, onPickSegment: selectSegment, onPickStation: selectStation, onEditStop });
  } catch (e) {
    status(`stop load failed: ${e.message}`);
  }
}

/// Compact popup on a viewport station marker; point pane mirrors the detail.
async function openStationPopup(stationId, marker) {
  try {
    const d = await api.stationDetail(stationId);
    const onPickRoute = (rid, dir) => selectRouteById(rid, dir ?? 0);
    paneOpen('point', `station:${stationId}`, () => selectStation(stationId));
    const el = stationPopupEl(d, { onPickRoute });
    marker.unbindPopup();
    marker.bindPopup(el, { maxWidth: 300 }).openPopup();
    stationPanel(d, { onPickRoute, onPickSegment: selectSegment, onPickStation: selectStation });
  } catch (e) {
    status(`station load failed: ${e.message}`);
  }
}

/// Search-driven station selection: pan + ring marker + popup; popup close removes it.
async function selectStation(stationId) {
  busy(true);
  try {
    const d = await api.stationDetail(stationId);
    const st = d.station;
    paneOpen('point', `station:${stationId}`, () => selectStation(stationId));
    clearHighlight();
    const m = L.circleMarker([st.lat, st.lon], {
      radius: 10, color: '#f59e0b', weight: 2, fillColor: '#f59e0b', fillOpacity: 0.15,
    }).addTo(layers.highlight);
    m.bindPopup(stationPopupEl(d, {
      onPickRoute: (rid, dir) => selectRouteById(rid, dir ?? 0),
    }), { maxWidth: 300 }).openPopup();
    m.on('popupclose', () => m.remove());
    map.panTo([st.lat, st.lon], { animate: true });
    stationPanel(d, { onPickRoute: (rid, dir) => selectRouteById(rid, dir ?? 0), onPickSegment: selectSegment, onPickStation: selectStation });
    location.hash = `#station=${stationId}`;
  } catch (e) {
    status(`station load failed: ${e.message}`);
  } finally {
    busy(false);
  }
}

/// Route-panel stop click: ring marker + compact popup + point pane, selectStation pattern.
async function popupStop(stopVersionId) {
  try {
    const d = await api.stopDetail(stopVersionId);
    const s = d.stop;
    const onPickRoute = (rid, dir) => selectRouteById(rid, dir ?? 0);
    paneOpen('point', `stop:${stopVersionId}`, () => selectStop(stopVersionId));
    clearHighlight();
    const m = L.circleMarker([s.lat, s.lon], {
      radius: 10, color: '#f59e0b', weight: 2, fillColor: '#f59e0b', fillOpacity: 0.15,
    }).addTo(layers.highlight);
    m.bindPopup(stopPopupEl(d, { onPickRoute }), { maxWidth: 300 }).openPopup();
    m.on('popupclose', () => m.remove());
    map.panTo([s.lat, s.lon], { animate: true });
    stopPanel(d, { onPickRoute, onPickSegment: selectSegment, onPickStation: selectStation, onEditStop });
  } catch (e) {
    status(`stop load failed: ${e.message}`);
  }
}

async function selectStop(stopVersionId) {
  busy(true);
  try {
    const d = await api.stopDetail(stopVersionId);
    paneOpen('point', `stop:${stopVersionId}`, () => selectStop(stopVersionId));
    highlightGroup(d.group_members);
    stopPanel(d, { onPickRoute: (rid, dir) => selectRouteById(rid, dir ?? 0), onPickSegment: selectSegment, onPickStation: selectStation, onEditStop });
    map.panTo([d.stop.lat, d.stop.lon], {
      animate: true,
    });
    location.hash = `#stop=${stopVersionId}`;
  } catch (e) {
    status(`stop load failed: ${e.message}`);
  } finally {
    busy(false);
  }
}

async function enableEditMode() {
  if (!sessionStorage.getItem('rm-edit-token')) {
    const tok = prompt('edit token:');
    if (!tok) return;
    sessionStorage.setItem('rm-edit-token', tok);
  }
  editMode = true;
  setEditable(true);
  document.getElementById('edit-toggle').classList.add('on');
  status('edit mode on');
}

function disableEditMode() {
  editMode = false;
  if (pendingPlace) {
    map.off('click', pendingPlace);
    pendingPlace = null;
  }
  setEditable(false);
  document.getElementById('edit-toggle').classList.remove('on');
  status('');
}

/// stop-panel edit actions: rename now, move = one-shot place mode.
function onEditStop(act, stop) {
  if (!editMode) return;
  if (act === 'rename') {
    const name = prompt(`rename stop #${stop.stop_code}:`, stop.stop_name);
    if (!name || name === stop.stop_name) return;
    const note = prompt('note (required):', '') ?? '';
    if (!note) return;
    api.editStop(stop.stop_version_id, { stop_name: name, note })
      .then((r) => { status('renamed'); return selectStop(r.new_stop_version_id); })
      .catch((e) => status(`rename failed: ${e.message}`));
  } else if (act === 'move') {
    status('click the map to place the stop');
    const handler = (e) => {
      pendingPlace = null;
      if (!editMode) return;
      const dist = Math.round(L.latLng([stop.lat, stop.lon]).distanceTo(e.latlng));
      if (!confirm(`move stop #${stop.stop_code} ${dist} m?\n(${stop.lat.toFixed(5)},${stop.lon.toFixed(5)}) → (${e.latlng.lat.toFixed(5)},${e.latlng.lng.toFixed(5)})`)) return status('');
      const note = prompt('note (required):', '') ?? '';
      if (!note) return status('');
      api.editStop(stop.stop_version_id, { lat: e.latlng.lat, lon: e.latlng.lng, note })
        .then((r) => { status(`moved ${dist} m`); return selectStop(r.new_stop_version_id); })
        .catch((err) => status(`move failed: ${err.message}`));
    };
    if (pendingPlace) map.off('click', pendingPlace);
    pendingPlace = handler;
    map.on('click', handler);
  }
}

async function openOutliers() {
  busy(true);
  try {
    const v = await api.outliers();
    paneOpen('context', 'outliers', openOutliers);
    outlierPanel(v.outliers ?? [], {
      onPick: (o) => {
        flashStop(o.lat, o.lon);
        //map.setView([o.lat, o.lon], Math.max(map.getZoom(), 15));
        map.panTo([o.lat, o.lon], { animate: true });
      },
      onOpenRoute: (o) => selectRouteById(o.route_id, o.direction),
    });
    location.hash = '#outliers';
  } catch (e) {
    status(`outliers load failed: ${e.message}`);
  } finally {
    busy(false);
  }
}

async function openMatchDevs() {
  busy(true);
  try {
    const v = await api.matchDevs();
    paneOpen('context', 'matchdev', openMatchDevs);
    matchDevPanel(v.directions ?? [], {
      onOpenRoute: (d) => selectRouteById(d.route_id, d.direction),
    });
    location.hash = '#matchdev';
  } catch (e) {
    status(`path dev load failed: ${e.message}`);
  } finally {
    busy(false);
  }
}

async function openVariants() {
  busy(true);
  try {
    const v = await api.variants();
    paneOpen('context', 'variants', openVariants);
    variantPanel(v.pairs ?? [], {
      onOpenSegment: (seg) => selectSegment(seg.segment_id),
    });
    location.hash = '#variants';
  } catch (e) {
    status(`seg variants load failed: ${e.message}`);
  } finally {
    busy(false);
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
    drawStopsLayer(v, {
      onStopClick: (svid, marker) => openStopPopup(svid, marker),
      onStationClick: (stid, marker) => openStationPopup(stid, marker),
    });
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
    const geom = document.getElementById('segment-geom').value;
    const v = await api.segmentsBbox(bbox, 0, geom);
    drawSegments(v, { onSegmentClick: selectSegment });
    if (v.truncated) status('segments truncated — zoom in');
  } catch (e) {
    status(`segments load failed: ${e.message}`);
  }
}

/// Segment drill-down: point pane + red highlight polyline, kept until the
/// point pane is closed or replaced (pane change hook clears it).
async function selectSegment(segmentId) {
  busy(true);
  try {
    const d = await api.segmentDetail(segmentId);
    paneOpen('point', `seg:${segmentId}`, () => selectSegment(segmentId));
    segmentPanel(d, {
      onPickRoute: (rid, dir) => selectRouteById(rid, dir ?? 0),
      onPickStation: selectStation,
    });
    if (d.geometry) drawSegmentHighlight(d.geometry);
    location.hash = `#segment=${segmentId}`;
  } catch (e) {
    status(`segment load failed: ${e.message}`);
  } finally {
    busy(false);
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
  setPaneChangeHook((kind, closed) => {
    if (closed) paneCurrent[kind] = null;
    if (kind === 'point') clearSegmentHighlight();
  });
  for (const kind of ['context', 'point']) {
    document.querySelector(`.dpane-${kind} .dpane-back`).addEventListener('click', () => paneBack(kind));
    document.querySelector(`.dpane-${kind} .dpane-fwd`).addEventListener('click', () => paneForward(kind));
  }
  // Esc: close popup → point pane → context pane. "/" focuses route search.
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      if (document.querySelector('.leaflet-popup')) map.closePopup();
      else if (!document.querySelector('.dpane-point').hidden) closePane('point');
      else if (!document.querySelector('.dpane-context').hidden) closePane('context');
    } else if (e.key === '/' && !/^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName ?? '')) {
      e.preventDefault();
      document.getElementById('route-search').focus();
    }
  });
  const search = document.getElementById('route-search');
  search.addEventListener('input', () => {
    refreshList(search.value);
    refreshStations(search.value);
  });
  // Hide/show the route-list sidebar; Leaflet must re-measure its container.
  const sidebarToggle = document.getElementById('sidebar-toggle');
  sidebarToggle.addEventListener('click', () => {
    const sb = document.getElementById('sidebar');
    sb.hidden = !sb.hidden;
    sidebarToggle.textContent = sb.hidden ? '»' : '«';
    sidebarToggle.title = sb.hidden ? 'show sidebar' : 'hide sidebar';
    map.invalidateSize();
  });
  document.getElementById('chip-outliers').addEventListener('click', openOutliers);
  document.getElementById('chip-matchdev').addEventListener('click', openMatchDevs);
  document.getElementById('chip-variants').addEventListener('click', openVariants);
  const segToggle = document.getElementById('layer-segments');
  segToggle.addEventListener('change', () => {
    if (segToggle.checked) refreshSegments();
    else clearSegments();
  });
  document.getElementById('segment-geom').addEventListener('change', () => {
    if (segToggle.checked) refreshSegments();
  });
  document.getElementById('route-geom').addEventListener('change', () => {
    if (selectedRoute) selectRouteById(selectedRoute.route_id, selectedRoute.dir);
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
  const bearingsToggle = document.getElementById('layer-bearings');
  bearingsToggle.addEventListener('change', () => {
    setBearingsOn(bearingsToggle.checked);
    if (stopsToggle.checked) refreshStopsLayer();
    if (lastRouteDraw) drawRoute(lastRouteDraw.p, lastRouteDraw.s, { onStopClick: lastRouteDraw.onStopClick, onPathClick: lastRouteDraw.onPathClick });
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
    setChip('chip-matchdev', s.match?.directions_dev_p95_over_50m ?? '—');
    setChip('chip-variants', s.variants?.split_pairs ?? '—');
    if (s.edit_enabled) {
      const btn = document.getElementById('edit-toggle');
      btn.hidden = false;
      btn.addEventListener('click', () => (editMode ? disableEditMode() : enableEditMode()));
    }
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
    } else if (h.station) {
      selectStation(Number(h.station));
    } else if (h.segment) {
      selectSegment(h.segment);
    }
  } catch (e) {
    status(`boot failed: ${e.message}`);
  }
}

boot();
