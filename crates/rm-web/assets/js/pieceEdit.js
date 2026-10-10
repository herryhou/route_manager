// Manual src-polyline piece editing (phase 1). One armed session at a time:
// click two raw vertices = span (the server enforces chainage order), then
// drag/insert/delete the span's vertices, save = POST .../piece. The server
// owns polyline math; this file only collects [lat,lon] pairs. Esc /
// edit-off tear down. src coords arrive GeoJSON lon-first and are
// normalized to [lat,lon] exactly once, here.
import { editPolylinePiece } from './api.js';

const VTX = (color) =>
  L.divIcon({
    className: '',
    html: `<div style="width:10px;height:10px;border-radius:50%;background:${color};border:2px solid #fff;box-shadow:0 0 2px #0008;"></div>`,
    iconSize: [10, 10],
    iconAnchor: [5, 5],
  });

let session = null;
// { map, group, line, src, routeId, dir, xno, pieceId, expectedUpdatedAt,
//   a, b, pts, btn, onSaved }

export function pieceEditActive() {
  return session != null;
}

export function stopPieceEdit() {
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
  // dimmed full src line (already [lat,lon])
  L.polyline(s.src, { color: '#7c3aed', weight: 2, dashArray: '2 7', opacity: 0.4 })
    .bindTooltip('src (raw)').addTo(s.group);
  // raw vertices: click = anchor pick
  s.src.forEach((p, i) => {
    L.marker(p, { icon: VTX('#7c3aed') }).addTo(s.group).on('click', () => pick(s, i));
  });
  if (s.b == null) return; // span not cut yet — nothing editable
  // current piece geometry, kept as a reference for in-place drag updates
  s.line = L.polyline(s.pts, { color: '#f59e0b', weight: 4, opacity: 0.95 }).addTo(s.group);
  // span vertices; endpoints are the anchors (pinned), interior are editable
  s.pts.forEach((p, i) => {
    if (i === 0 || i === s.pts.length - 1) {
      L.marker(p, { icon: VTX('#334155') }).addTo(s.group)
        .bindTooltip(i === 0 ? 'anchor start' : 'anchor end');
      return;
    }
    const m = L.marker(p, { icon: VTX('#f59e0b'), draggable: true }).addTo(s.group);
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
    L.marker(mid, {
      icon: L.divIcon({
        className: '',
        html: '<div style="width:8px;height:8px;background:#fff;border:2px solid #f59e0b;border-radius:50%;"></div>',
        iconSize: [8, 8], iconAnchor: [4, 4],
      }),
    }).addTo(s.group).on('click', () => {
      s.pts.splice(i + 1, 0, mid);
      render(s);
    });
  }
}

function pick(s, i) {
  if (s.a == null || s.b != null) {
    // fresh pick (or re-pick after a complete span)
    s.a = i; s.b = null; s.pts = [s.src[i]];
  } else if (i !== s.a) {
    const [lo, hi] = i > s.a ? [s.a, i] : [i, s.a];
    s.a = lo; s.b = hi;
    s.pts = s.src.slice(lo, hi + 1);
  }
  render(s);
}

export function startPieceEdit(routeId, dir, xno, raw, piece, onSaved) {
  const coords = raw?.path?.geometry?.coordinates ?? [];
  if (coords.length < 2) return;
  stopPieceEdit();
  const map = window.__rmMap;
  const src = coords.map(([lon, lat]) => [lat, lon]); // GeoJSON lon-first → bus [lat,lon], once
  const group = L.layerGroup().addTo(map);
  map.doubleClickZoom.disable();
  session = {
    map, group, line: null, src, routeId, dir, xno,
    pieceId: piece?.piece_id ?? null,
    expectedUpdatedAt: piece?.updated_at ?? null,
    a: null, b: null, pts: [], btn: null, onSaved,
  };
  // editing an existing piece: its endpoints snap to the nearest raw
  // vertices so the span logic has indices; the piece's own geometry is
  // what gets edited (anchors re-sent from pts endpoints — pinned, so they
  // still equal the stored anchors within SEAM_TOL_M)
  if (piece) {
    const near = (pt) => src.reduce(
      (best, q, i) => {
        const d = (q[0] - pt[0]) ** 2 + (q[1] - pt[1]) ** 2;
        return d < best.d ? { d, i } : best;
      }, { d: Infinity, i: 0 }).i;
    session.a = near(piece.anchor_start);
    session.b = near(piece.anchor_end);
    if (session.a > session.b) [session.a, session.b] = [session.b, session.a];
    session.pts = piece.points.map((p) => [p[0], p[1]]);
  }
  const btn = document.createElement('div');
  btn.style.cssText = 'position:absolute;bottom:56px;right:12px;z-index:1000;display:flex;gap:6px;';
  btn.innerHTML = `
    <button id="piece-save" style="padding:4px 10px;cursor:pointer;">✓ 存_piece</button>
    <button id="piece-cancel" style="padding:4px 10px;cursor:pointer;">✕</button>`;
  document.body.appendChild(btn);
  session.btn = btn;
  btn.querySelector('#piece-cancel').onclick = () => stopPieceEdit();
  btn.querySelector('#piece-save').onclick = () => save(session);
  render(session);
}

async function save(s) {
  if (s.b == null || s.pts.length < 2) return;
  const note = prompt('piece note:');
  if (note == null) return; // cancelled
  try {
    const out = await editPolylinePiece(s.xno, s.dir, {
      points: s.pts,
      anchor_start: s.pts[0],
      anchor_end: s.pts[s.pts.length - 1],
      piece_id: s.pieceId ?? undefined,
      expected_updated_at: s.expectedUpdatedAt ?? undefined,
      note,
    });
    const done = s.onSaved;
    stopPieceEdit();
    done?.(out);
  } catch (e) {
    s.onSaved?.(null, e);
  }
}
