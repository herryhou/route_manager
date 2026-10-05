// Sidebar + detail panel rendering.
import { snapColor, matchedBadge, METHOD_COLORS } from './colors.js';

export function renderRouteList(routes, { onSelect, selectedKey }) {
  const list = document.getElementById('route-list');
  list.innerHTML = '';
  for (const r of routes) {
    const row = document.createElement('div');
    row.className = 'route-row' + (selectedKey === `${r.route_id}` ? ' selected' : '');
    const code = document.createElement('span');
    code.className = 'route-code';
    code.textContent = r.route_code;
    const name = document.createElement('span');
    name.className = 'route-name';
    name.textContent = r.subroute_name ?? r.ext_uid;
    name.title = `${r.subroute_name ?? ''} · ${r.providers.join(', ') || 'no provider'}`;
    row.append(code, name);
    for (const d of r.directions ?? []) {
      const dot = document.createElement('span');
      dot.className = 'dir-dot';
      dot.style.background = snapColor(d.snap_dist_p50_m);
      dot.title = `dir ${d.direction}: ${d.stops} stops, p50 ${d.snap_dist_p50_m?.toFixed(1) ?? '—'}m, ${matchedBadge(d.matched_ratio).text}`;
      row.append(dot);
    }
    row.addEventListener('click', () => onSelect(r));
    list.append(row);
  }
}

/// Sidebar station search results (above the route list); empty list hides the box.
export function renderStationList(stations, { onSelect }) {
  const box = document.getElementById('station-list');
  box.innerHTML = '';
  box.hidden = !stations.length;
  if (!stations.length) return;
  const head = document.createElement('div');
  head.className = 'list-head';
  head.textContent = `Stations (${stations.length})`;
  box.append(head);
  for (const s of stations) {
    const row = document.createElement('div');
    row.className = 'station-row';
    const name = document.createElement('span');
    name.className = 'station-name';
    name.textContent = s.station_name;
    name.title = s.station_name;
    row.append(name);
    row.addEventListener('click', () => onSelect(s));
    box.append(row);
  }
}

export function filterRoutes(routes, q) {
  q = q.trim();
  if (!q) return routes;
  return routes.filter(
    (r) =>
      r.route_code.startsWith(q) ||
      (r.subroute_name ?? '').includes(q) ||
      r.providers.some((p) => p.includes(q))
  );
}

/// Detail panel = two stacked panes, latest only (no tabs): context on top
/// (route stops / outliers / segment), point detail below (stop drill-down).
/// Re-showing a pane replaces its content and re-wires listeners.
const PANE_KINDS = ['context', 'point'];

function paneEl(kind) {
  return document.getElementById('detail').querySelector(`.dpane-${kind}`);
}

export function showPane(kind, html, title) {
  const pane = paneEl(kind);
  document.getElementById('detail').hidden = false;
  pane.hidden = false;
  pane.querySelector('.dpane-title').textContent = title;
  pane.querySelector('.dpane-close').onclick = () => closePane(kind);
  const body = pane.querySelector('.dpane-body');
  body.innerHTML = html;
  body.scrollTop = 0;
  return body;
}

export function closePane(kind) {
  const pane = paneEl(kind);
  pane.hidden = true;
  if (PANE_KINDS.every((k) => paneEl(k).hidden)) {
    document.getElementById('detail').hidden = true;
  }
}

export function kv(pairs) {
  const rows = pairs
    .map(([k, v]) => `<tr><td>${k}</td><td>${v ?? '—'}</td></tr>`)
    .join('');
  return `<table class="kv">${rows}</table>`;
}

export function routeChips(routes, onPick) {
  const wrap = document.createElement('span');
  for (const r of routes) {
    const chip = document.createElement('span');
    chip.className = 'chip-route';
    chip.textContent = r.route_code;
    chip.title = r.subroute_name ?? '';
    chip.addEventListener('click', () => onPick(r.route_id, r.direction));
    wrap.append(chip);
  }
  return wrap;
}

/// Route detail panel: header with direction toggle + ordered stop list.
export function routeStopsPanel(d, dir, stops, { onDirection, onStopClick, onFlashStop }) {
  const dirBtns = (d.directions ?? [])
    .map(
      (x) =>
        `<button data-dir="${x.direction}" class="${x.direction === dir ? 'active' : ''}">${
          x.direction === 0 ? '去程 (0)' : '返程 (1)'
        }</button>`
    )
    .join('');
  const rows = (stops.stops ?? [])
    .map((s) => {
      const badge =
        s.method === 'OUTLIER' || s.method === 'NO_PATH'
          ? `<span class="badge bad">${s.method}</span>`
          : s.snap_dist_m != null && s.snap_dist_m > 50
            ? `<span class="badge warn">${s.snap_dist_m.toFixed(0)}m</span>`
            : '';
      return `<div class="stop-row" data-svid="${s.stop_version_id}" data-lat="${s.lat}" data-lon="${s.lon}">
        <span class="stop-seq">${s.stop_sequence}</span>
        <span class="stop-name" title="${s.stop_name} · snap ${s.snap_dist_m?.toFixed(1) ?? '—'}m">${s.stop_name}</span>
        ${badge}
      </div>`;
    })
    .join('');
  const html = `
    <div class="tabs">${dirBtns}</div>
    ${kv([
      ['providers', d.providers.join(', ')],
      ['version', `v${d.version_no} · ${d.status}`],
      ['side routes', d.side_routes.join(', ')],
      ['main routes', d.main_routes.join(', ')],
    ])}
    <div class="tabs view-tabs">
      <button data-view="stops" class="active">Stops (${(stops.stops ?? []).length})</button>
      <button data-view="qa">QA profile</button>
    </div>
    <div class="view-stops">${rows}</div>
    <div class="view-qa" hidden>${profileSvg(stops.stops ?? [])}</div>
  `;
  const pane = showPane('context', html, `Route ${d.route_code} ${d.subroute_name ?? ''}`.trim());
  pane.querySelectorAll('.tabs:not(.view-tabs) button').forEach((b) =>
    b.addEventListener('click', () => onDirection(Number(b.dataset.dir)))
  );
  pane.querySelectorAll('.view-tabs button').forEach((b) =>
    b.addEventListener('click', () => {
      pane.querySelectorAll('.view-tabs button').forEach((x) => x.classList.remove('active'));
      b.classList.add('active');
      pane.querySelector('.view-stops').hidden = b.dataset.view !== 'stops';
      pane.querySelector('.view-qa').hidden = b.dataset.view !== 'qa';
    })
  );
  pane.querySelectorAll('.view-stops .stop-row').forEach((row) =>
    row.addEventListener('click', () => onStopClick(Number(row.dataset.svid)))
  );
}

/// SVG snap-distance profile: x = chainage, y = snap distance (capped at CAP_M;
/// off-scale dots clamp to the top edge and carry the true value in <title>).
const PROFILE_CAP_M = 150;
export function profileSvg(stops) {
  if (!stops.length) return '<p class="muted">no stops</p>';
  const W = 340;
  const H = 120;
  const PAD = { l: 34, r: 6, t: 8, b: 16 };
  const maxCh = Math.max(...stops.map((s) => s.chainage_m ?? 0), 1);
  const x = (ch) => PAD.l + ((ch ?? 0) / maxCh) * (W - PAD.l - PAD.r);
  const y = (snap) =>
    H - PAD.b - (Math.min(snap ?? 0, PROFILE_CAP_M) / PROFILE_CAP_M) * (H - PAD.t - PAD.b);
  const grid = [0, 15, 40, PROFILE_CAP_M]
    .map(
      (m) =>
        `<line x1="${PAD.l}" x2="${W - PAD.r}" y1="${y(m)}" y2="${y(m)}" stroke="#e5e7eb" stroke-width="1"/>
         <text x="${PAD.l - 4}" y="${y(m) + 3}" text-anchor="end" font-size="8" fill="#6b7280">${m}m</text>`
    )
    .join('');
  const dots = stops
    .map(
      (s) => `<circle cx="${x(s.chainage_m)}" cy="${y(s.snap_dist_m)}" r="3"
        fill="${METHOD_COLORS[s.method] ?? '#7da7d9'}" fill-opacity="0.9">
        <title>${s.stop_sequence}. ${s.stop_name} — snap ${s.snap_dist_m?.toFixed(1) ?? '—'}m (${s.method})</title>
      </circle>`
    )
    .join('');
  return `
    <p class="muted" style="margin:0 0 4px">snap distance along route (cap ${PROFILE_CAP_M}m; green &lt;15, amber &lt;40)</p>
    <svg viewBox="0 0 ${W} ${H}" width="100%" style="display:block">
      ${grid}${dots}
    </svg>`;
}

/// Segment detail panel: corridor card + ways table + traversing routes.
export function segmentPanel(d, { onPickRoute }) {
  const ways = (d.ways ?? [])
    .map(
      (w) => `<tr><td>${w.ordinal}</td><td>${w.name ?? '—'}</td><td>${w.highway ?? '—'}</td>
        <td style="text-align:right">${w.length_m.toFixed(0)}m</td></tr>`
    )
    .join('');
  const pane = showPane('context', `
    ${kv([
      ['length', `${d.length_m.toFixed(0)} m`],
      ['ways', d.way_count],
      ['start', `${d.start_station.station_name ?? d.start_station.station_id}`],
      ['end', `${d.end_station.station_name ?? d.end_station.station_id}`],
    ])}
    <h3>Ways (${(d.ways ?? []).length})</h3>
    <table class="kv"><tr><td>#</td><td>name</td><td>highway</td><td style="text-align:right">len</td></tr>${ways}</table>
    <h3>Routes (${(d.routes ?? []).length})</h3>
    <div class="segment-routes"></div>
  `, `Segment ${d.segment_id.slice(0, 12)}…`);
  pane.querySelector('.segment-routes').append(routeChips(d.routes ?? [], onPickRoute));
}

/// Outlier list panel (topbar outliers chip).
export function outlierPanel(rows, { onPick, onOpenRoute }) {
  const fmt = (m) => (m >= 1000 ? `${(m / 1000).toFixed(1)}km` : `${m.toFixed(0)}m`);
  const items = (rows ?? [])
    .map(
      (o) => `<div class="stop-row outlier-row" title="${o.stop_name} · ${o.method} · snap ${fmt(o.snap_dist_m)}">
        <span class="route-code">${o.route_code}</span>
        <span class="stop-name">${o.stop_name}</span>
        <span class="badge ${o.method === 'OUTLIER' ? 'bad' : 'warn'}">${fmt(o.snap_dist_m)}</span>
      </div>`
    )
    .join('');
  const pane = showPane('context', `
    <p class="muted">click to locate; route code opens the route</p>
    ${items || '<p class="muted">none</p>'}
  `, `Outliers (${(rows ?? []).length})`);
  pane.querySelectorAll('.outlier-row').forEach((row, i) => {
    row.addEventListener('click', () => onPick(rows[i]));
    row.querySelector('.route-code').addEventListener('click', (e) => {
      e.stopPropagation();
      onOpenRoute(rows[i]);
    });
  });
}

/// North-up bearing dial — magnified version of the map's dot+arrow glyph.
/// Single arrow = stop (direction of travel); axis = station (headings fold
/// onto one axis, so the arrow is double-headed); spread draws the member-
/// deviation wedge, warn-tinted past the rm-verify 45° threshold.
function dialSvg(b, { axis = false, spread = null, color = 'var(--accent)' } = {}) {
  const C = 22, R = 18;
  const pol = (deg, r) => {
    const t = (deg * Math.PI) / 180;
    return `${(C + r * Math.sin(t)).toFixed(1)},${(C - r * Math.cos(t)).toFixed(1)}`;
  };
  let wedge = '';
  if (b != null && spread != null) {
    const s = Math.min(Math.max(spread, 0.6), 89);
    wedge = `<path d="M${C},${C} L${pol(b - s, R)} A${R},${R} 0 0 1 ${pol(b + s, R)} Z"
      fill="${spread > 45 ? 'var(--warn)' : color}" opacity="0.16"/>`;
  }
  const c = b == null ? 'var(--muted)' : color;
  const glyph = b == null ? '' : `<g transform="rotate(${b} ${C} ${C})">
    ${axis ? `<line x1="${C}" y1="${C + 12}" x2="${C}" y2="${C - 9}" stroke="${c}" stroke-width="2"/>` : ''}
    <line x1="${C}" y1="${C}" x2="${C}" y2="${C - 9}" stroke="${c}" stroke-width="2"/>
    <path d="M${C},${C - 16} L${C - 3.4},${C - 8.5} L${C},${C - 10.5} L${C + 3.4},${C - 8.5} Z" fill="${c}"/>
    ${axis ? `<path d="M${C},${C + 16} L${C - 3.4},${C + 8.5} L${C},${C + 10.5} L${C + 3.4},${C + 8.5} Z" fill="${c}"/>` : ''}
  </g>`;
  return `<svg class="pp-dial" viewBox="0 0 44 44" width="36" height="36" role="img"
    aria-label="${b == null ? 'no bearing' : `bearing ${Math.round(b)}°`}">
    <circle cx="${C}" cy="${C}" r="${R}" fill="white" stroke="var(--border)"/>
    <line x1="${C}" y1="1.5" x2="${C}" y2="4" stroke="var(--muted)" stroke-width="1.5"/>
    ${wedge}${glyph}
    <circle cx="${C}" cy="${C}" r="2" fill="${c}"/>
  </svg>`;
}

// Direction variants repeat route_code — popups show one chip per code+name.
const uniqRoutes = (rs) => [...new Map((rs ?? []).map((r) => [`${r.route_code}|${r.subroute_name ?? ''}`, r])).values()];

/// Station card shared by both popups: axis dial + name + degrees (spread
/// only when it trips the rm-verify 45° warn threshold), member stop codes
/// (top-level only), station-serving route chips.
function stationCard({ station, bearing, routes, stops }, { onPickRoute }) {
  const el = document.createElement('div');
  const spread = bearing?.max_spread_deg;
  const spreadTxt = spread != null && spread > 45
    ? ` · <span class="pp-hot">spread ${Math.round(spread)}°</span>`
    : '';
  // Direction variants repeat the same stop_code — unique codes only.
  const uniqCodes = [...new Map((stops ?? []).map((s) => [s.stop_code, s])).values()];
  const codes = uniqCodes.length
    ? `<div class="pp-meta">stops ${uniqCodes.map((s) =>
        `<span class="pp-code" title="${s.stop_name}">#${s.stop_code}</span>`).join(' ')}</div>`
    : '';
  el.innerHTML = `
    <div class="pp-head">
      ${dialSvg(bearing?.bearing_deg, { axis: true, spread, color: 'var(--amber)' })}
      <div>
        <div class="pp-title">${station.station_name} <span class="pp-code">#${station.station_id}</span></div>
        <div class="pp-meta">${bearing ? `axis <b>${Math.round(bearing.bearing_deg)}°</b>${spreadTxt}` : 'no bearing'}</div>
      </div>
    </div>
    ${codes}
    <div class="pp-chips"></div>
  `;
  el.querySelector('.pp-chips').append(routeChips(uniqRoutes(routes), onPickRoute));
  return el;
}

/// Compact stop popup (map marker click / route-panel click): dial + name,
/// route chips under a 經過路線 label — full detail lives in the point pane.
export function stopPopupEl(d, { onPickRoute }) {
  const wrap = document.createElement('div');
  wrap.className = 'stop-popup';
  wrap.innerHTML = `
    <div class="pp-head">
      ${dialSvg(d.bearing?.bearing_deg, { color: 'var(--accent)' })}
      <div>
        <div class="pp-title">${d.stop.stop_name} <span class="pp-code">#${d.stop.stop_code}</span></div>
      </div>
    </div>
    <div class="pp-label">經過路線</div>
    <div class="pp-chips"></div>
  `;
  wrap.querySelector('.pp-chips').append(routeChips(uniqRoutes(d.routes_by_stop), onPickRoute));
  return wrap;
}

/// Compact station popup (search list / viewport station marker click).
export function stationPopupEl(d, { onPickRoute }) {
  const wrap = document.createElement('div');
  wrap.className = 'stop-popup';
  wrap.append(
    stationCard(
      { station: d.station, group: d.group, bearing: d.bearing, routes: d.routes, stops: d.stops },
      { onPickRoute }
    )
  );
  return wrap;
}

/// Station detail panel (point pane): axis/spread kv, member stops, routes.
export function stationPanel(d, { onPickRoute }) {
  const stops = d.stops ?? [];
  const rows = stops
    .map((s) => `<div class="stop-row" title="${s.stop_name}">
        <span class="stop-seq">#${s.stop_code}</span>
        <span class="stop-name">${s.stop_name}</span>
      </div>`)
    .join('');
  const pane = showPane('point', `
    ${kv([
      ['axis', d.bearing ? `${Math.round(d.bearing.bearing_deg)}°` : '—'],
      ['spread', d.bearing ? `${Math.round(d.bearing.max_spread_deg)}°` : '—'],
      ['members', d.bearing?.member_count ?? stops.length],
      ['group', d.group ? d.group.station_group_name : '—'],
    ])}
    <h3>Stops (${stops.length})</h3>
    ${rows}
    <h3>Routes (${uniqRoutes(d.routes).length})</h3>
    <div class="station-routes"></div>
  `, `${d.station.station_name} #${d.station.station_id}`);
  pane.querySelector('.station-routes').append(routeChips(uniqRoutes(d.routes), onPickRoute));
  return pane;
}

/// Stop drill-down panel: stop → station → group + serving routes.
export function stopPanel(d, { onPickRoute }) {
  const pane = showPane('point', `
    ${kv([
      ['stop', `${d.stop.stop_name} (${d.stop.stop_code})`],
      ['bearing', d.bearing ? `${Math.round(d.bearing.bearing_deg)}° · ${d.bearing.source}` : '—'],
      ['station', d.station.station_name],
      ['station bearing', d.station_bearing
        ? `${Math.round(d.station_bearing.bearing_deg)}° · spread ${Math.round(d.station_bearing.max_spread_deg)}° · ${d.station_bearing.member_count} members`
        : '—'],
      ['group', `${d.group.station_group_name} · ${d.group.source}`],
    ])}
    <h3>Group members (${(d.group_members ?? []).length} stations)</h3>
    <div class="group-chips"></div>
    <h3>Routes serving this stop (${(d.routes_by_stop ?? []).length})</h3>
    <div class="routes-stop-chips"></div>
    <h3>Routes serving station (${(d.routes_by_station ?? []).length})</h3>
    <div class="routes-station-chips"></div>
    <h3>Routes serving group (${(d.routes_by_group ?? []).length})</h3>
    <div class="routes-group-chips"></div>
  `, `${d.stop.stop_name} #${d.stop.stop_code}`);
  pane.querySelector('.group-chips').append(
    ...d.group_members.map((m) => {
      const el = document.createElement('span');
      el.className = 'chip-route';
      el.textContent = `站 ${m.station_id}`;
      el.title = m.station_name;
      return el;
    })
  );
  for (const [cls, routes] of [
    ['routes-stop-chips', d.routes_by_stop],
    ['routes-station-chips', d.routes_by_station],
    ['routes-group-chips', d.routes_by_group],
  ]) {
    pane.querySelector(`.${cls}`).append(routeChips(routes, onPickRoute));
  }
}
