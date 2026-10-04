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

/// Tabbed detail panel: each { key, title } is one tab; content panes stay in
/// the DOM (listeners intact) so flipping back needs no refetch. Re-showing a
/// key replaces its content and activates it.
export function showDetail(html, { key = 'main', title = 'detail' } = {}) {
  const el = document.getElementById('detail');
  let strip = el.querySelector('.detail-tabs');
  if (!strip) {
    el.replaceChildren();
    strip = document.createElement('div');
    strip.className = 'detail-tabs';
    const closeAll = document.createElement('button');
    closeAll.className = 'close';
    closeAll.title = 'close panel';
    closeAll.textContent = '✕';
    closeAll.addEventListener('click', hideDetail);
    strip.append(closeAll);
    el.append(strip);
  }
  let pane = el.querySelector(`[data-pane="${key}"]`);
  if (pane) {
    pane.replaceChildren();
  } else {
    pane = document.createElement('div');
    pane.dataset.pane = key;
    pane.hidden = true;
    el.append(pane);
    const tab = document.createElement('button');
    tab.className = 'detail-tab';
    tab.dataset.key = key;
    const label = document.createElement('span');
    label.textContent = title;
    const x = document.createElement('span');
    x.className = 'tab-x';
    x.textContent = '✕';
    tab.append(label, x);
    tab.addEventListener('click', (e) => {
      if (e.target === x) closeDetailTab(key);
      else activateDetailTab(key);
    });
    strip.insertBefore(tab, strip.querySelector('.close'));
  }
  pane.innerHTML = html;
  activateDetailTab(key);
  return pane;
}

function activateDetailTab(key) {
  const el = document.getElementById('detail');
  el.hidden = false;
  for (const p of el.querySelectorAll('[data-pane]')) p.hidden = p.dataset.pane !== key;
  for (const t of el.querySelectorAll('.detail-tab')) t.classList.toggle('active', t.dataset.key === key);
}

function closeDetailTab(key) {
  const el = document.getElementById('detail');
  const pane = el.querySelector(`[data-pane="${key}"]`);
  const wasActive = !pane?.hidden;
  pane?.remove();
  el.querySelector(`.detail-tab[data-key="${key}"]`)?.remove();
  const remaining = [...el.querySelectorAll('.detail-tab')];
  if (!remaining.length) hideDetail();
  else if (wasActive) activateDetailTab(remaining[remaining.length - 1].dataset.key);
}

export function hideDetail() {
  const el = document.getElementById('detail');
  el.replaceChildren();
  el.hidden = true;
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
    <h2>Route ${d.route_code} <small class="muted">${d.subroute_name ?? ''}</small></h2>
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
  const pane = showDetail(html, { key: `route-${d.route_id}`, title: d.route_code });
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
  const pane = showDetail(`
    <h2>Segment <small class="muted">${d.segment_id.slice(0, 12)}…</small></h2>
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
  `, { key: `segment-${d.segment_id}`, title: `seg ${d.segment_id.slice(0, 6)}` });
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
  const pane = showDetail(`
    <h2>Outliers <small class="muted">${(rows ?? []).length}</small></h2>
    <p class="muted">click to locate; route code opens the route</p>
    ${items || '<p class="muted">none</p>'}
  `, { key: 'outliers', title: 'outliers' });
  pane.querySelectorAll('.outlier-row').forEach((row, i) => {
    row.addEventListener('click', () => onPick(rows[i]));
    row.querySelector('.route-code').addEventListener('click', (e) => {
      e.stopPropagation();
      onOpenRoute(rows[i]);
    });
  });
}

/// Compact stop popup (map marker click): summary + stop-serving routes + link to full panel.
export function stopPopupEl(d, { onDetails, onPickRoute }) {
  const wrap = document.createElement('div');
  wrap.className = 'stop-popup';
  const bearing = d.bearing
    ? `${Math.round(d.bearing.bearing_deg)}° · ${d.bearing.source}`
    : '—';
  wrap.innerHTML = `
    <div class="popup-title">${d.stop.stop_name} <small class="muted">#${d.stop.stop_code}</small></div>
    <div class="muted">bearing ${bearing}</div>
    <div class="muted">station ${d.station.station_name}</div>
    <div class="popup-routes muted">routes (${(d.routes_by_stop ?? []).length})</div>
  `;
  const chips = document.createElement('div');
  chips.append(routeChips(d.routes_by_stop ?? [], onPickRoute));
  const btn = document.createElement('button');
  btn.className = 'popup-details';
  btn.textContent = 'more details…';
  btn.addEventListener('click', onDetails);
  wrap.append(chips, btn);
  return wrap;
}

/// Compact station popup (viewport station marker click).
export function stationPopupEl(d, { onPickRoute }) {
  const wrap = document.createElement('div');
  wrap.className = 'stop-popup';
  const bearing = d.bearing
    ? `${Math.round(d.bearing.bearing_deg)}° · spread ${Math.round(d.bearing.max_spread_deg)}° · ${d.bearing.member_count} members`
    : '—';
  wrap.innerHTML = `
    <div class="popup-title">${d.station.station_name}</div>
    <div class="muted">bearing ${bearing}</div>
    <div class="muted">group ${d.group.station_group_name}</div>
    <div class="muted">stops (${(d.stops ?? []).length}): ${(d.stops ?? []).map((s) => s.stop_code).join(' ')}</div>
    <div class="popup-routes muted">routes (${(d.routes ?? []).length})</div>
  `;
  const chips = document.createElement('div');
  chips.append(routeChips(d.routes ?? [], onPickRoute));
  wrap.append(chips);
  return wrap;
}

/// Stop drill-down panel: stop → station → group + serving routes.
export function stopPanel(d, { onPickRoute }) {
  const pane = showDetail(`
    <h2>${d.stop.stop_name} <small class="muted">#${d.stop.stop_code}</small></h2>
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
  `, { key: `stop-${d.stop.stop_version_id}`, title: `#${d.stop.stop_code}` });
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
