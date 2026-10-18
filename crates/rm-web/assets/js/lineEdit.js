// Whole-line override editing (phase 2). One armed session at a time: the
// effective line (override ?? src) renders fully editable — drag any vertex,
// dblclick to delete, press a midpoint to insert (keep holding to drag the
// new vertex); save = POST .../line
// replacing the entire direction polyline. The server owns polyline math;
// this file only collects [lat,lon] pairs. Esc / edit-off tear down.
// src coords arrive GeoJSON lon-first and are normalized to [lat,lon]
// exactly once, here; effective.points are already [lat,lon].
import { editPolylineLine } from './api.js';
import { I } from './icons.js';

const VTX = (color) =>
  L.divIcon({
    className: '',
    html: `<div style="width:10px;height:10px;border-radius:50%;background:${color};border:2px solid #fff;box-shadow:0 0 2px #0008;"></div>`,
    iconSize: [10, 10],
    iconAnchor: [5, 5],
  });

let session = null;
// { map, group, line, src, routeId, dir, xno, pts, expectedUpdatedAt, btn, onSaved }

export function lineEditActive() {
  return session != null;
}

export function stopLineEdit() {
  if (!session) return;
  session.map.doubleClickZoom.enable();
  session.group.remove();
  session.btn?.remove();
  session = null;
}

// Full rebuild of the session layer. NEVER called while a vertex marker is
// mid-drag: drag updates the line in place via setLatLngs; dragend (or any
// discrete op) triggers the rebuild. clearLayers() during a drag destroys
// the dragged marker and kills the gesture.
function render(s) {
  s.group.clearLayers();
  s.line = null;
  // dimmed raw capture line (already [lat,lon]) — reference, not editable
  L.polyline(s.src, { color: '#16150f', weight: 2, dashArray: '2 7', opacity: 0.4 })
    .bindTooltip('src (raw)').addTo(s.group);
  // the line being edited (the effective line, possibly already moved off src)
  s.line = L.polyline(s.pts, { color: '#d9531e', weight: 4, opacity: 0.95 }).addTo(s.group);
  s.vms = [];
  s.pts.forEach((p, i) => {
    // zIndexOffset keeps session markers above the stops/bearings layer —
    // app.js redraws that layer on every moveend, which re-appends its
    // icons after ours and would otherwise bury any handle it overlaps.
    const m = L.marker(p, { icon: VTX('#d9531e'), draggable: true, zIndexOffset: 1000 }).addTo(s.group);
    s.vms[i] = m;
    m.on('drag', (e) => {
      s.pts[i] = [e.latlng.lat, e.latlng.lng];
      s.line.setLatLngs(s.pts);
    });
    m.on('dragend', () => render(s));
    m.on('dblclick', () => {
      if (s.pts.length > 2) { s.pts.splice(i, 1); render(s); }
    });
  });
  // midpoint insert handles
  for (let i = 0; i < s.pts.length - 1; i++) {
    const mid = [(s.pts[i][0] + s.pts[i + 1][0]) / 2, (s.pts[i][1] + s.pts[i + 1][1]) / 2];
    const h = L.marker(mid, {
      icon: L.divIcon({
        className: '',
        html: '<div style="width:8px;height:8px;background:#fff;border:2px solid #d9531e;border-radius:50%;"></div>',
        iconSize: [8, 8], iconAnchor: [4, 4],
      }),
      // one above the vertex handles: Leaflet orders same-pane markers by
      // screen y, so without this a vertex 1px higher buries a midpoint in
      // self-overlapping runs (loops) and clicks there never insert.
      zIndexOffset: 1001,
    }).addTo(s.group);
    // Press = insert, on a raw mousedown: (1) stopPropagation keeps the
    // gesture away from the map pane — otherwise mousedown arms map-drag and
    // any micro-movement pans instead of inserting (Leaflet also suppresses
    // the click via draggableMoved); (2) the DOM click event dies whenever
    // the mouse drifts off the 8px handle between press and release (click
    // fires on the press/release common ancestor), so click is not a
    // reliable trigger here. Vertex markers are immune — their own Draggable
    // claims the gesture first.
    L.DomEvent.on(h.getElement(), 'mousedown', (e) => {
      L.DomEvent.stopPropagation(e);
      s.pts.splice(i + 1, 0, mid);
      render(s);
      // render() rebuilt the layer after this mousedown, so the inserted
      // vertex was born mid-gesture: it never saw a mousedown and its own
      // Draggable is unarmed. Carry the still-held gesture to it by hand —
      // otherwise the vertex can only be moved after release + re-press.
      followInsertDrag(s, i + 1);
    });
  }
}

// Document-level continuation of a press-insert: mousemove moves the new
// vertex (marker + line, same as the vertex drag handlers), mouseup does the
// one rebuild. Listeners live on document because the marker element the
// press started on is gone.
function followInsertDrag(s, i) {
  const m = s.vms[i];
  const move = (e) => {
    const ll = s.map.mouseEventToLatLng(e);
    s.pts[i] = [ll.lat, ll.lng];
    m.setLatLng(ll);
    s.line.setLatLngs(s.pts);
  };
  const up = () => {
    L.DomEvent.off(document, 'mousemove', move);
    L.DomEvent.off(document, 'mouseup', up);
    render(s);
  };
  L.DomEvent.on(document, 'mousemove', move);
  L.DomEvent.on(document, 'mouseup', up);
}

export function startLineEdit(routeId, dir, xno, raw, onSaved) {
  const coords = raw?.path?.geometry?.coordinates ?? [];
  const eff = raw?.effective;
  if (coords.length < 2 || (eff?.points?.length ?? 0) < 2) {
    // Say why instead of dying silently — a degenerate payload here reads
    // as "修線壞了" (a short full-line override + 更新 produces exactly this).
    window.__rmStatus?.('無法修線：此方向的有效線少於 2 點');
    return;
  }
  stopLineEdit();
  const map = window.__rmMap;
  const src = coords.map(([lon, lat]) => [lat, lon]); // GeoJSON lon-first → bus [lat,lon], once
  const group = L.layerGroup().addTo(map);
  map.doubleClickZoom.disable();
  session = {
    map, group, line: null, src, routeId, dir, xno,
    // edit the effective line; echo its token only when it IS an override
    // (editing a src line creates one — there is no row to race yet)
    pts: eff.points.map((p) => [p[0], p[1]]),
    expectedUpdatedAt: eff.source === 'override' ? eff.updated_at : null,
    btn: null, onSaved,
  };
  const btn = document.createElement('div');
  btn.style.cssText = 'position:absolute;bottom:56px;right:12px;z-index:1000;display:flex;gap:6px;';
  btn.innerHTML = `
    <button id="line-save" class="le-btn le-save">${I.check} 存線</button>
    <button id="line-cancel" class="le-btn">${I.close}</button>`;
  document.body.appendChild(btn);
  session.btn = btn;
  btn.querySelector('#line-cancel').onclick = () => stopLineEdit();
  btn.querySelector('#line-save').onclick = () => save(session);
  render(session);
}

async function save(s) {
  if (s.pts.length < 2) return;
  const note = prompt('線段備註:');
  if (note == null) return; // cancelled
  try {
    const out = await editPolylineLine(s.xno, s.dir, {
      points: s.pts,
      expected_updated_at: s.expectedUpdatedAt ?? undefined,
      note,
    });
    const done = s.onSaved;
    stopLineEdit();
    done?.(out);
  } catch (e) {
    s.onSaved?.(null, e);
  }
}
