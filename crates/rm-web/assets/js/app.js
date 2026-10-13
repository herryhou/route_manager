// Bootstrap + selection flow: route → direction → stops → stop drill-down.
import * as api from './api.js';
import { renderRouteList, renderStationList, filterRoutes, routeStopsPanel, stopPanel, stationPanel, stopPopupEl, stationPopupEl, outlierPanel, matchDevPanel, editLogPanel, pathPiecePanel, setPaneChangeHook, pushPaneHistory, popPaneHistory, popPaneForward, closePane, setEditable, esc } from './panels.js';
import { initLayers, layers, clearHighlight, drawRoute, highlightGroup, flashStop, drawRaw, clearRaw, drawStopsLayer, clearStopsLayer, setBearingsOn, setEffectiveOn, drawPieceHighlight, clearPieceHighlight } from './layers.js';
import { startLineEdit, stopLineEdit, lineEditActive } from './lineEdit.js';
import { I } from './icons.js';

export const map = L.map('map', { preferCanvas: true }).fitBounds([
  [23.974, 120.498],
  [24.447, 121.322],
]);

L.tileLayer(window.RM_TILES, {
  maxZoom: 23,
  attribution: '&copy; OpenStreetMap contributors',
}).addTo(map);

initLayers(map);

// lineEdit.js reaches the map/session status through these globals.
window.__rmMap = map;
window.__rmStatus = status;

export function status(msg) {
  document.getElementById('status-msg').textContent = msg ?? '';
}

/// Spinner in the statusbar — on while a primary fetch runs (route/station/
/// QA panel loads), off in finally. Background layers don't touch it.
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
let selectedRoute = null; // { route_id, dir, xno }
let lastRouteDraw = null; // { p, s, onStopClick } — for bearing-toggle redraw
let lastRaw = null; // { routeId, dir, v } — for line-edit entry
let editMode = false;
let refreshTimer = null;
let pendingPlace = null; // armed move-place map handler — replaced/cancelled, never stacked
let lineBtn = null; // 修線 + 線史 — created in boot() when edit is enabled
let lineLogBtn = null;

/// Shared visibility gate for the line-edit entries: edit mode + raw layer
/// + a raw payload on screen. Also tears an armed session down with edit-off.
function syncLineBtn() {
  const show = editMode && document.getElementById('layer-raw').checked && lastRaw ? '' : 'none';
  if (lineBtn) lineBtn.style.display = show;
  if (lineLogBtn) lineLogBtn.style.display = show;
  if (!editMode) stopLineEdit();
}

function onLineSaved(out, err) {
  if (err) { status(`線段儲存失敗: ${err.message}`); return; }
  status(out.created ? '已建立線段覆寫' : '已更新線段覆寫');
  if (lastRaw && document.getElementById('layer-raw').checked) loadRaw(lastRaw.routeId, lastRaw.dir);
}

/// Re-draw the raw layer from the cached payload (raw/effective toggle
/// redraws share this closure).
function redrawRaw() {
  drawRaw(lastRaw.v, {
    onEditLine: () => {
      if (!editMode || lineEditActive()) return;
      startLineEdit(lastRaw.routeId, lastRaw.dir, selectedRoute.xno, lastRaw.v, onLineSaved);
    },
  });
}

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
      status(`站點搜尋失敗: ${e.message}`);
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
  else status(`路線 ${routeId} 不在清單`);
}

/// Way-run properties per `route_id:dir`, cached for piece-panel lookup —
/// the same data the ways overlay draws, fetched once per direction.
const wayRunsCache = {};
async function wayRuns(routeId, dir) {
  const key = `${routeId}:${dir}`;
  if (!wayRunsCache[key]) {
    const v = await api.routeWays(routeId, dir);
    wayRunsCache[key] = (v.features ?? []).map((f) => f.properties);
  }
  return wayRunsCache[key];
}

/// Fill the piece panel's `.piece-ways` container: the ordered OSM way runs
/// covering the draft piece's stop-to-stop chainage window (the per-route
/// derivation that replaced the segment registry). `stops` is captured at
/// click time — pane-history replays must not read another route's stops.
/// Async — the panel is already shown; ways land when the fetch resolves.
async function fillPieceWays(el, routeId, dir, stops, p) {
  const from = stops.find((x) => x.stop_sequence === p.from_seq);
  const to = stops.find((x) => x.stop_sequence === p.to_seq);
  const lo = from?.chainage_m;
  const hi = to?.chainage_m;
  if (lo == null || hi == null || hi <= lo) {
    el.innerHTML = '<p class="muted">無里程區間</p>';
    return;
  }
  try {
    const runs = await wayRuns(routeId, dir);
    const fmt = (m) => (m >= 1000 ? `${(m / 1000).toFixed(1)}km` : `${m.toFixed(0)}m`);
    const covering = runs
      .map((r) => ({ ...r, overlap: Math.min(r.cum_end_m, hi) - Math.max(r.cum_start_m, lo) }))
      .filter((r) => r.overlap > 0.5)
      .sort((a, b) => a.cum_start_m - b.cum_start_m);
    el.innerHTML = covering.length
      ? `<table class="kv">${covering
          .map(
            (r) =>
              `<tr><td>${esc(r.name ?? '—')} (#${r.osm_way_id})</td><td style="text-align:right">${fmt(r.overlap)}</td></tr>`
          )
          .join('')}</table>`
      : '<p class="muted">無覆蓋的 OSM 街道</p>';
  } catch (e) {
    el.innerHTML = `<p class="muted">街道載入失敗: ${esc(e.message)}</p>`;
  }
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
    selectedRoute.xno = d.xno; // piece-edit POSTs key on it; only detail carries it
    const onStopClick = (stop, marker) => openStopPopup(stop.stop_version_id, marker);
    // Path click: point pane + red highlight of that piece / way run (pane
    // change hook clears it). The reopen closure replays it from the captured
    // properties — no refetch.
    const onPathClick = (feature) => {
      const p = feature.properties;
      const dirStops = s.stops ?? [];
      const show = () => {
        const waysEl = pathPiecePanel(p);
        if (waysEl) fillPieceWays(waysEl, route_id, dir, dirStops, p);
        if (feature.geometry) {
          drawPieceHighlight(feature.geometry, p.osm_way_id != null ? 'selected way' : 'selected piece');
        }
      };
      paneOpen('point', `piece:${route_id}:${dir}:${p.ordinal}:${p.osm_way_id ?? 'draft'}`, show);
      show();
    };
    lastRouteDraw = { p, s, onStopClick, onPathClick };
    drawRoute(p, s, { onStopClick, onPathClick });
    if (document.getElementById('layer-raw').checked) loadRaw(route_id, dir);
    else clearRaw();
    paneOpen('context', `route:${route_id}:${dir}`, () => selectRouteById(route_id, dir));
    routeStopsPanel(d, dir, s, {
      onDirection: (nd) => selectRoute(r, nd),
      onStopClick: (svid) => popupStop(svid),
    });
    if ((p.features ?? []).length === 0) status('此方向無路徑資料');
    location.hash = `#route=${route_id}&dir=${dir}`;
  } catch (e) {
    status(`路線載入失敗: ${e.message}`);
  } finally {
    busy(false);
  }
}

async function loadRaw(routeId, dir) {
  try {
    const v = await api.routeRaw(routeId, dir);
    if (selectedRoute?.route_id !== routeId || selectedRoute.dir !== dir) return; // stale
    lastRaw = { routeId, dir, v };
    redrawRaw();
    syncLineBtn(); // entries may have been waiting for this payload
  } catch (e) {
    status(`原始資料載入失敗: ${e.message}`);
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
    stopPanel(d, { onPickRoute, onPickStation: selectStation, onEditStop });
  } catch (e) {
    status(`站牌載入失敗: ${e.message}`);
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
    stationPanel(d, { onPickRoute, onPickStation: selectStation, onEditStation });
  } catch (e) {
    status(`站點載入失敗: ${e.message}`);
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
      radius: 10, color: '#0857a2', weight: 2, fillColor: '#0857a2', fillOpacity: 0.15,
    }).addTo(layers.highlight);
    m.bindPopup(stationPopupEl(d, {
      onPickRoute: (rid, dir) => selectRouteById(rid, dir ?? 0),
    }), { maxWidth: 300 }).openPopup();
    m.on('popupclose', () => m.remove());
    map.panTo([st.lat, st.lon], { animate: true });
    stationPanel(d, { onPickRoute: (rid, dir) => selectRouteById(rid, dir ?? 0), onPickStation: selectStation, onEditStation });
    location.hash = `#station=${stationId}`;
  } catch (e) {
    status(`站點載入失敗: ${e.message}`);
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
      radius: 10, color: '#0857a2', weight: 2, fillColor: '#0857a2', fillOpacity: 0.15,
    }).addTo(layers.highlight);
    m.bindPopup(stopPopupEl(d, { onPickRoute }), { maxWidth: 300 }).openPopup();
    m.on('popupclose', () => m.remove());
    map.panTo([s.lat, s.lon], { animate: true });
    stopPanel(d, { onPickRoute, onPickStation: selectStation, onEditStop });
  } catch (e) {
    status(`站牌載入失敗: ${e.message}`);
  }
}

async function selectStop(stopVersionId) {
  busy(true);
  try {
    const d = await api.stopDetail(stopVersionId);
    paneOpen('point', `stop:${stopVersionId}`, () => selectStop(stopVersionId));
    highlightGroup(d.group_members);
    stopPanel(d, { onPickRoute: (rid, dir) => selectRouteById(rid, dir ?? 0), onPickStation: selectStation, onEditStop });
    map.panTo([d.stop.lat, d.stop.lon], {
      animate: true,
    });
    location.hash = `#stop=${stopVersionId}`;
  } catch (e) {
    status(`站牌載入失敗: ${e.message}`);
  } finally {
    busy(false);
  }
}

async function enableEditMode() {
  if (!sessionStorage.getItem('rm-edit-token')) {
    const tok = prompt('編輯權杖:');
    if (!tok) return;
    sessionStorage.setItem('rm-edit-token', tok);
  }
  editMode = true;
  setEditable(true);
  document.getElementById('edit-toggle').classList.add('on');
  document.getElementById('edit-toggle').setAttribute('aria-pressed', 'true');
  status('編輯模式開啟');
  document.getElementById('refresh-btn').hidden = false;
  // Re-seed the pending-count badge: the boot seed may be stale (no token
  // yet, or marks created in a previous session).
  api.refreshStatus().then((v) => {
    setChip('refresh-btn', v.pending_marks > 0 ? v.pending_marks : '—');
  }).catch(() => {});
}

function disableEditMode() {
  editMode = false;
  if (pendingPlace) {
    map.off('click', pendingPlace);
    pendingPlace.cancel?.();
    pendingPlace = null;
  }
  setEditable(false);
  document.getElementById('edit-toggle').classList.remove('on');
  document.getElementById('edit-toggle').setAttribute('aria-pressed', 'false');
  status('');
  document.getElementById('refresh-btn').hidden = true;
  document.getElementById('refresh-btn').disabled = false;
  clearInterval(refreshTimer);
  refreshTimer = null;
}

/// Esc aborts an armed place action (move/regroup): the handler's cancel
/// tears down its guide line + cursor; the status bar clears with it.
document.addEventListener('keydown', (e) => {
  if (e.key !== 'Escape' || !pendingPlace) return;
  pendingPlace.cancel?.();
  status('');
});

// Esc tears down an armed piece-edit session (independent of panes/popups).
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') stopLineEdit();
});

/// Arm a one-shot map-click place action (move/regroup): crosshair cursor
/// + elastic dashed guide from `from` following the mouse until the click
/// lands, Esc/edit-off/another arm tears it down. `onClick` runs after the
/// teardown with the leaflet click event.
function armPlace(from, onClick) {
  const mapEl = map.getContainer();
  const guide = L.polyline([from, from], {
    color: '#d9531e', weight: 2, dashArray: '6 6', interactive: false,
  }).addTo(map);
  mapEl.style.cursor = 'crosshair';
  const follow = (e) => guide.setLatLngs([from, e.latlng]);
  const handler = (e) => {
    handler.cancel();
    onClick(e);
  };
  handler.cancel = () => {
    map.off('click', handler); // self-deregister: one-shot again
    map.off('mousemove', follow);
    guide.remove();
    mapEl.style.cursor = '';
    if (pendingPlace === handler) pendingPlace = null;
  };
  if (pendingPlace) {
    map.off('click', pendingPlace);
    pendingPlace.cancel?.();
  }
  pendingPlace = handler;
  map.on('click', handler);
  map.on('mousemove', follow);
}

/// Post-edit reload: re-fetch what's on screen so the new version shows —
/// the selected route's markers + stop list, and the viewport stops layer.
async function refreshAfterEdit() {
  if (selectedRoute) await selectRouteById(selectedRoute.route_id, selectedRoute.dir);
  if (document.getElementById('layer-stops').checked) refreshStopsLayer();
}

/// Edit failure → status bar; a rejected token also drops the stored one, so
/// the next edit-mode toggle re-prompts instead of looping 403s.
function editFail(what, e) {
  if (/HTTP 40[13]/.test(e.message)) sessionStorage.removeItem('rm-edit-token');
  status(`${what}失敗: ${e.message}`);
}

/// stop-panel edit actions: rename now, move/regroup = one-shot place mode,
/// log = edit-history panel with per-row undo.
function onEditStop(act, stop, stationId) {
  if (!editMode) return;
  if (act === 'log') {
    openEditLog(stop);
  } else if (act === 'rename') {
    const name = prompt(`rename stop #${stop.stop_code}:`, stop.stop_name);
    if (!name || name === stop.stop_name) return;
    const note = prompt('備註（必填）:', '') ?? '';
    if (!note) return;
    api.editStop(stop.stop_version_id, { stop_name: name, note })
      .then(async (r) => {
        status('已改名');
        await refreshAfterEdit();
        return selectStop(r.new_stop_version_id);
      })
      .catch((e) => editFail('rename', e));
  } else if (act === 'regroup') {
    status('點擊目標站點');
    armPlace([stop.lat, stop.lon], (e) => {
      if (!editMode) return;
      api.stationNearest(e.latlng.lat, e.latlng.lng, 100)
        .then((hit) => {
          if (hit.station_id === stationId) return status(`already at ${hit.station_name}`);
          if (!confirm(`改歸站點？站牌 #${stop.stop_code}\n${stop.stop_name} → ${hit.station_name}（${Math.round(hit.distance_m)} 公尺）`)) return status('');
          const note = prompt('備註（必填）:', '') ?? '';
          if (!note) return status('');
          return api.regroupStop(stop.stop_version_id, { new_station_id: hit.station_id, note })
            .then(async (r) => {
              status('已改歸站點');
              await refreshAfterEdit();
              return selectStop(r.new_stop_version_id);
            });
        })
        .catch((err) => {
          if (/HTTP 404/.test(err.message)) status('100 公尺內無站點');
          else editFail('regroup', err);
        });
    });
  } else if (act === 'move') {
    status('點擊地圖放置站牌');
    armPlace([stop.lat, stop.lon], (e) => {
      if (!editMode) return;
      const dist = Math.round(L.latLng([stop.lat, stop.lon]).distanceTo(e.latlng));
      if (!confirm(`搬移站牌 #${stop.stop_code} ${dist} 公尺？\n(${stop.lat.toFixed(5)},${stop.lon.toFixed(5)}) → (${e.latlng.lat.toFixed(5)},${e.latlng.lng.toFixed(5)})`)) return status('');
      const note = prompt('備註（必填）:', '') ?? '';
      if (!note) return status('');
      api.editStop(stop.stop_version_id, { lat: e.latlng.lat, lon: e.latlng.lng, note })
        .then(async (r) => {
          status(`已搬移 ${dist} 公尺`);
          await refreshAfterEdit();
          return selectStop(r.new_stop_version_id);
        })
        .catch((err) => editFail('move', err));
    });
  }
}

/// Edit-history panel for a stop (↩ 歷史 mini-btn): audit rows newest
/// first; undoable rows carry an ↩ in edit mode. stop.stop_id keys the
/// log — the version id churns on every edit, the stop id never does.
async function openEditLog(stop) {
  busy(true);
  try {
    const v = await api.editLog('stop', stop.stop_id);
    paneOpen('context', `editlog:${stop.stop_id}`, () => openEditLog(stop));
    editLogPanel(v.rows ?? [], {
      canUndo: editMode,
      onUndo: (editId) => undoStopEdit(stop, editId),
    });
  } catch (e) {
    status(`編輯歷史載入失敗: ${e.message}`);
  } finally {
    busy(false);
  }
}

/// Undo one logged stop edit: confirm → note → POST undo → reload what's
/// on screen at the restored version, then re-fetch the log. A 409
/// NotUndoable/NotCurrent (stale hint, row already undone) lands in the
/// status bar via editFail.
async function undoStopEdit(stop, editId) {
  if (!editMode) return;
  if (!confirm(`復原站牌 #${stop.stop_code} 的編輯 #${editId}？`)) return;
  const note = prompt('備註（必填）:', '') ?? '';
  if (!note) return;
  try {
    const r = await api.undoEdit(editId, note);
    status('已復原');
    await refreshAfterEdit();
    await selectStop(r.new_stop_version_id);
    await openEditLog(stop); // fresh rows — the undo row is now newest
  } catch (e) {
    editFail('undo', e);
  }
}

/// station-panel edit actions: rename (prompted name + pole level),
/// move = one-shot place mode, log = station edit history with per-row undo.
function onEditStation(act, d) {
  if (!editMode) return;
  const st = d.station;
  if (act === 'log') {
    openStationEditLog(st);
  } else if (act === 'rename') {
    const name = prompt(`rename station #${st.station_id}:`, st.station_name);
    if (!name || name === st.station_name) return;
    const stops = d.stops ?? [];
    const matches = stops.filter((s) => s.stop_name === st.station_name).length;
    const lv = prompt(
      `層級:\n1 只改站點\n2 同名站牌 (${matches}/${stops.length} 根相符)\n3 全部站牌（預設）`, '3');
    if (!lv) return;
    const level = { 1: 'station', 2: 'matching', 3: 'all' }[lv.trim()];
    if (!level) return status(`層級無效: ${lv}`);
    const reach = level === 'all' ? ` (${stops.length} 根站牌)`
      : level === 'matching' ? ` (${matches} 根站牌)` : '';
    if (!confirm(`${st.station_name} → ${name}\n層級: ${level}${reach}`)) return status('');
    const note = prompt('備註（必填）:', '') ?? '';
    if (!note) return status('');
    api.renameStation(st.station_version_id, { new_name: name, level, note })
      .then(async () => {
        status('已改名');
        await refreshAfterEdit();
        return selectStation(st.station_id);
      })
      .catch((e) => editFail('rename', e));
  } else if (act === 'move') {
    status('點擊新的站點位置');
    armPlace([st.lat, st.lon], (e) => {
      if (!editMode) return;
      const dist = Math.round(L.latLng([st.lat, st.lon]).distanceTo(e.latlng));
      if (!confirm(`搬移站點 #${st.station_id} ${dist} 公尺？\n站牌不動\n(${st.lat.toFixed(5)},${st.lon.toFixed(5)}) → (${e.latlng.lat.toFixed(5)},${e.latlng.lng.toFixed(5)})`)) return status('');
      const note = prompt('備註（必填）:', '') ?? '';
      if (!note) return status('');
      api.moveStation(st.station_version_id, { lat: e.latlng.lat, lon: e.latlng.lng, note })
        .then(async () => {
          status('已搬移');
          await refreshAfterEdit();
          return selectStation(st.station_id);
        })
        .catch((err) => editFail('move', err));
    });
  }
}

/// Edit-history panel for a station (↩ 歷史 mini-btn): station edit rows
/// newest first; station_id keys the log — station_version_id churns on
/// every edit, the station id never does.
async function openStationEditLog(st) {
  busy(true);
  try {
    const v = await api.editLog('station', st.station_id);
    paneOpen('context', `stationeditlog:${st.station_id}`, () => openStationEditLog(st));
    editLogPanel(v.rows ?? [], {
      canUndo: editMode,
      onUndo: (editId) => undoStationEdit(st, editId),
    });
  } catch (e) {
    status(`編輯歷史載入失敗: ${e.message}`);
  } finally {
    busy(false);
  }
}

/// Undo one logged station edit: confirm → note → POST undo (the dispatched
/// undo route restores the station row) → reload what's on screen, then
/// re-fetch the log. A 409 lands in the status bar via editFail.
async function undoStationEdit(st, editId) {
  if (!editMode) return;
  if (!confirm(`復原站點 #${st.station_id} 的編輯 #${editId}？`)) return;
  const note = prompt('備註（必填）:', '') ?? '';
  if (!note) return;
  try {
    const r = await api.undoEdit(editId, note);
    status('已復原');
    await refreshAfterEdit();
    await selectStation(st.station_id);
    await openStationEditLog(st); // fresh rows — the undo row is now newest
  } catch (e) {
    editFail('undo', e);
  }
}

/// Edit-history panel for the selected route's line overrides (線史):
/// polyline edit rows key on route_id (entity='polyline', entity_id=route).
/// ↩ deletes the override (undo of its create) or restores the prior line.
async function openLineLog(routeId) {
  busy(true);
  try {
    const v = await api.editLog('polyline', routeId);
    paneOpen('context', `linelog:${routeId}`, () => openLineLog(routeId));
    editLogPanel(v.rows ?? [], {
      canUndo: editMode,
      onUndo: (editId) => undoLineEdit(routeId, editId),
    });
  } catch (e) {
    status(`線段歷史載入失敗: ${e.message}`);
  } finally {
    busy(false);
  }
}

/// Undo one logged line edit. The polyline branch of /api/edit/undo replies
/// {edit_id, restored: {deleted}} — deleted means the override's create was
/// reverted (the row is gone, back to pure src), otherwise the prior line
/// is back. Refresh = raw re-fetch (the effective line may have moved),
/// then the log for the fresh rows.
async function undoLineEdit(routeId, editId) {
  if (!editMode) return;
  if (!confirm(`復原路線 #${routeId} 的線段編輯 #${editId}？`)) return;
  const note = prompt('備註（必填）:', '') ?? '';
  if (!note) return;
  try {
    const r = await api.undoEdit(editId, note);
    status(r.restored?.deleted ? '已刪除線段覆寫（回到原始線）' : '已還原線段覆寫');
    if (lastRaw && document.getElementById('layer-raw').checked) loadRaw(lastRaw.routeId, lastRaw.dir);
    await openLineLog(routeId); // fresh rows — the undo row is now newest
  } catch (e) {
    editFail('undo', e);
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
    status(`離群站牌載入失敗: ${e.message}`);
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
    status(`路徑偏離載入失敗: ${e.message}`);
  } finally {
    busy(false);
  }
}

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
    if (v.truncated) status(`${kind === 'stop' ? '站牌' : '站點'}圖層已截斷 — 請放大`);
  } catch (e) {
    status(`站牌圖層載入失敗: ${e.message}`);
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
  // Icon slots: index.html carries no inline SVG — fill every [data-icon]
  // from icons.js (one stroke, one weight across the whole UI).
  for (const el of document.querySelectorAll('[data-icon]')) {
    el.innerHTML = I[el.dataset.icon] ?? '';
  }
  setPaneChangeHook((kind, closed) => {
    if (closed) paneCurrent[kind] = null;
    if (kind === 'point') clearPieceHighlight();
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
    sidebarToggle.innerHTML = sb.hidden ? I.panelR : I.panelL;
    sidebarToggle.title = sb.hidden ? '展開側欄' : '收合側欄';
    map.invalidateSize();
  });
  document.getElementById('chip-outliers').addEventListener('click', openOutliers);
  document.getElementById('chip-matchdev').addEventListener('click', openMatchDevs);
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
    clearTimeout(stopsTimer);
    stopsTimer = setTimeout(refreshStopsLayer, 250);
  });
  // Legend: the map's line encodings, named. Static — the encodings are
  // world constants (layers.js), not user state. Floats on the map
  // (bottom-left); dash swatches mirror the real dashArray values. Blue is
  // the matched path; orange (有效線) is the override ?? src line the raw
  // layer draws — the sidebar 有效線 toggle gates the orange one.
  document.getElementById('map-legend').innerHTML = [
    ['比對路徑（路線）', '#0857a2'],
    ['原始比對線', '#16150f', '2 7'],
    ['有效線（比對用）', '#d9531e'],
    ['站點環', '#e8a013', 'ring'],
  ]
    .map(([label, c, enc]) => {
      const style =
        enc === 'ring'
          ? `width:9px;height:9px;border:2px solid ${c};background:none;border-radius:50%`
          : enc
            ? (() => {
                const [a, b] = enc.split(' ');
                return `background:repeating-linear-gradient(90deg,${c} 0 ${a}px,transparent ${a}px ${+a + +b}px)`;
              })()
            : `background:${c}`;
      return `<div class="legend-row"><span class="legend-swatch" style="${style}"></span>${label}</div>`;
    })
    .join('');

  try {
    const [s, rv] = await Promise.all([api.stats(), api.routes()]);
    const c = s.counts ?? {};
    setChip('chip-routes', c.routes ?? '—');
    setChip('chip-stops', c.stops ?? '—');
    setChip('chip-stations', c.stations ?? '—');
    setChip('chip-match', `${s.match?.directions_with_stats ?? '—'}/${s.match?.directions_total ?? '—'}`);
    setChip('chip-outliers', s.outlier_stops ?? '—');
    setChip('chip-matchdev', s.match?.directions_dev_p95_over_50m ?? '—');
    if (s.edit_enabled) {
      const btn = document.getElementById('edit-toggle');
      btn.hidden = false;
      const rbtn = document.getElementById('refresh-btn');
      rbtn.hidden = false;
      // Seed the pending-count badge. The button stays ENABLED at 0
      // pending (a no-op run is harmless) — edits made later in the
      // session have no push channel, so a disabled-at-0 rule would
      // brick the button after new edits (spec deviation, flagged).
      api.refreshStatus().then((v) => {
        setChip('refresh-btn', v.pending_marks > 0 ? v.pending_marks : '—');
      }).catch(() => {});
      rbtn.addEventListener('click', async () => {
        if (refreshTimer) return; // a run is already in flight
        if (!confirm('套用更新？將對所有過期編輯重跑比對、投影與方位。')) return;
        try {
          await api.refreshApply();
        } catch (e) {
          status(`套用失敗: ${e.message}`);
          return;
        }
        rbtn.disabled = true;
        refreshTimer = setInterval(async () => {
          try {
            const v = await api.refreshStatus();
            if (v.state === 'running') {
              rbtn.querySelector('b').textContent = v.step;
              return;
            }
            clearInterval(refreshTimer);
            refreshTimer = null;
            rbtn.disabled = false;
            setChip('refresh-btn', v.pending_marks > 0 ? v.pending_marks : '—');
            if (v.state === 'failed') status(`套用失敗: ${v.error}`);
            else status(v.pending_marks > 0 ? `更新已套用，保留 ${v.pending_marks} 筆待標` : '更新已套用');
            // Derived data changed server-side (failed runs may still have
            // committed earlier steps): drop the piece-panel way-run cache
            // and redraw what's on screen.
            for (const k of Object.keys(wayRunsCache)) delete wayRunsCache[k];
            refreshAfterEdit();
            syncLineBtn(); // line-btn visibility may change with the redraw
          } catch (e) {
            clearInterval(refreshTimer);
            refreshTimer = null;
            rbtn.disabled = false;
            status(`套用狀態輪詢失敗: ${e.message}`);
          }
        }, 2000);
      });
      // 修線 entry + 線史 (line edit log): visible only with edit mode +
      // raw layer + a raw payload up (syncLineBtn).
      lineBtn = document.createElement('button');
      lineBtn.id = 'line-edit-btn';
      lineBtn.textContent = '修線';
      lineBtn.className = 'band-btn';
      lineBtn.style.cssText = 'display:none;';
      lineBtn.addEventListener('click', () => {
        if (!lastRaw || lineEditActive()) return;
        startLineEdit(lastRaw.routeId, lastRaw.dir, selectedRoute.xno, lastRaw.v, onLineSaved);
      });
      btn.parentElement.appendChild(lineBtn);
      lineLogBtn = document.createElement('button');
      lineLogBtn.id = 'line-log-btn';
      lineLogBtn.textContent = '線史';
      lineLogBtn.title = 'line edit history';
      lineLogBtn.className = 'band-btn';
      lineLogBtn.style.cssText = 'display:none;';
      lineLogBtn.addEventListener('click', () => {
        if (!lastRaw) return;
        openLineLog(lastRaw.routeId);
      });
      btn.parentElement.appendChild(lineLogBtn);
      document.getElementById('layer-raw').addEventListener('change', syncLineBtn);
      const effToggle = document.getElementById('layer-effective');
      effToggle.addEventListener('change', () => {
        setEffectiveOn(effToggle.checked);
        if (lastRaw && document.getElementById('layer-raw').checked) redrawRaw();
      });
      btn.addEventListener('click', () => {
        if (editMode) disableEditMode();
        else enableEditMode();
        syncLineBtn();
      });
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
    }
  } catch (e) {
    status(`啟動失敗: ${e.message}`);
  }
}

boot();
