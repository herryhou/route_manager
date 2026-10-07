// Bootstrap + selection flow: route → direction → stops → stop drill-down.
import * as api from './api.js';
import { renderRouteList, renderStationList, filterRoutes, routeStopsPanel, stopPanel, stationPanel, stopPopupEl, stationPopupEl, outlierPanel, matchDevPanel, variantPanel, segmentPanel, pathPiecePanel, setPaneChangeHook } from './panels.js';
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

function setChip(id, text) {
  const el = document.getElementById(id);
  if (el) el.querySelector('b').textContent = text;
}

let allRoutes = [];
let selectedKey = null;
let selectedRoute = null; // { route_id, dir }
let lastRouteDraw = null; // { p, s, onStopClick } — for bearing-toggle redraw

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
    const geom = document.getElementById('route-geom').value;
    const [d, p, s] = await Promise.all([
      api.routeDetail(route_id),
      geom === 'osm' ? api.routeWays(route_id, dir) : api.routePath(route_id, dir),
      api.routeStops(route_id, dir),
    ]);
    if (selectedRoute?.route_id !== route_id || selectedRoute.dir !== dir) return; // stale
    const onStopClick = (stop, marker) => openStopPopup(stop.stop_version_id, marker);
    // Path click: point pane + red highlight of that piece / way run, same
    // selectSegment pattern (pane change hook clears it).
    const onPathClick = (feature) => {
      pathPiecePanel(feature.properties, { onPickSegment: selectSegment });
      if (feature.geometry) {
        drawSegmentHighlight(
          feature.geometry,
          feature.properties.osm_way_id != null ? 'selected way' : 'selected piece'
        );
      }
    };
    lastRouteDraw = { p, s, onStopClick, onPathClick };
    drawRoute(p, s, { onStopClick, onPathClick });
    if (document.getElementById('layer-raw').checked) loadRaw(route_id, dir);
    else clearRaw();
    fitRoute(p, s);
    routeStopsPanel(d, dir, s, {
      onDirection: (nd) => selectRoute(r, nd),
      onStopClick: (svid) => popupStop(svid),
    });
    if ((p.features ?? []).length === 0) status('no path data for this direction');
    location.hash = `#route=${route_id}&dir=${dir}`;
  } catch (e) {
    status(`route load failed: ${e.message}`);
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
    const el = stopPopupEl(d, { onPickRoute });
    marker.unbindPopup();
    marker.bindPopup(el, { maxWidth: 300 }).openPopup();
    stopPanel(d, { onPickRoute, onPickSegment: selectSegment, onPickStation: selectStation });
  } catch (e) {
    status(`stop load failed: ${e.message}`);
  }
}

/// Compact popup on a viewport station marker; point pane mirrors the detail.
async function openStationPopup(stationId, marker) {
  try {
    const d = await api.stationDetail(stationId);
    const onPickRoute = (rid, dir) => selectRouteById(rid, dir ?? 0);
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
  try {
    const d = await api.stationDetail(stationId);
    const st = d.station;
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
  }
}

/// Route-panel stop click: ring marker + compact popup + point pane, selectStation pattern.
async function popupStop(stopVersionId) {
  try {
    const d = await api.stopDetail(stopVersionId);
    const s = d.stop;
    const onPickRoute = (rid, dir) => selectRouteById(rid, dir ?? 0);
    clearHighlight();
    const m = L.circleMarker([s.lat, s.lon], {
      radius: 10, color: '#f59e0b', weight: 2, fillColor: '#f59e0b', fillOpacity: 0.15,
    }).addTo(layers.highlight);
    m.bindPopup(stopPopupEl(d, { onPickRoute }), { maxWidth: 300 }).openPopup();
    m.on('popupclose', () => m.remove());
    map.panTo([s.lat, s.lon], { animate: true });
    stopPanel(d, { onPickRoute, onPickSegment: selectSegment, onPickStation: selectStation });
  } catch (e) {
    status(`stop load failed: ${e.message}`);
  }
}

async function selectStop(stopVersionId) {
  try {
    const d = await api.stopDetail(stopVersionId);
    highlightGroup(d.group_members);
    stopPanel(d, { onPickRoute: (rid, dir) => selectRouteById(rid, dir ?? 0), onPickSegment: selectSegment, onPickStation: selectStation });
    map.panTo([d.stop.lat, d.stop.lon], {
      animate: true,
    });
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
        //map.setView([o.lat, o.lon], Math.max(map.getZoom(), 15));
        map.panTo([o.lat, o.lon], { animate: true });
      },
      onOpenRoute: (o) => selectRouteById(o.route_id, o.direction),
    });
    location.hash = '#outliers';
  } catch (e) {
    status(`outliers load failed: ${e.message}`);
  }
}

async function openMatchDevs() {
  try {
    const v = await api.matchDevs();
    matchDevPanel(v.directions ?? [], {
      onOpenRoute: (d) => selectRouteById(d.route_id, d.direction),
    });
    location.hash = '#matchdev';
  } catch (e) {
    status(`path dev load failed: ${e.message}`);
  }
}

async function openVariants() {
  try {
    const v = await api.variants();
    variantPanel(v.pairs ?? [], {
      onOpenSegment: (seg) => selectSegment(seg.segment_id),
    });
    location.hash = '#variants';
  } catch (e) {
    status(`seg variants load failed: ${e.message}`);
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
  try {
    const d = await api.segmentDetail(segmentId);
    segmentPanel(d, {
      onPickRoute: (rid, dir) => selectRouteById(rid, dir ?? 0),
      onPickStation: selectStation,
    });
    if (d.geometry) drawSegmentHighlight(d.geometry);
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
  setPaneChangeHook((kind) => {
    if (kind === 'point') clearSegmentHighlight();
  });
  const search = document.getElementById('route-search');
  search.addEventListener('input', () => {
    refreshList(search.value);
    refreshStations(search.value);
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
