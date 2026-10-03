// Sidebar + detail panel rendering.
import { snapColor, matchedBadge } from './colors.js';

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

export function showDetail(html) {
  const el = document.getElementById('detail');
  el.innerHTML = `<button class="close" title="close">✕</button>` + html;
  el.hidden = false;
  el.querySelector('.close').addEventListener('click', hideDetail);
}

export function hideDetail() {
  document.getElementById('detail').hidden = true;
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
  showDetail(`
    <h2>Route ${d.route_code} <small class="muted">${d.subroute_name ?? ''}</small></h2>
    <div class="tabs">${dirBtns}</div>
    ${kv([
      ['providers', d.providers.join(', ')],
      ['version', `v${d.version_no} · ${d.status}`],
      ['side routes', d.side_routes.join(', ')],
      ['main routes', d.main_routes.join(', ')],
    ])}
    <h3>Stops (${(stops.stops ?? []).length})</h3>
    <div id="stop-rows">${rows}</div>
  `);
  document.querySelectorAll('#detail .tabs button').forEach((b) =>
    b.addEventListener('click', () => onDirection(Number(b.dataset.dir)))
  );
  document.querySelectorAll('#detail .stop-row').forEach((row) =>
    row.addEventListener('click', () => onStopClick(Number(row.dataset.svid)))
  );
}

/// Stop drill-down panel: stop → station → group + serving routes.
export function stopPanel(d, { onPickRoute }) {
  showDetail(`
    <h2>${d.stop.stop_name} <small class="muted">#${d.stop.stop_code}</small></h2>
    ${kv([
      ['stop', `${d.stop.stop_name} (${d.stop.stop_code})`],
      ['station', d.station.station_name],
      ['group', `${d.group.station_group_name} · ${d.group.source}`],
    ])}
    <h3>Group members (${(d.group_members ?? []).length} stations)</h3>
    <div id="group-chips"></div>
    <h3>Routes serving this stop (${(d.routes_by_stop ?? []).length})</h3>
    <div id="routes-stop-chips"></div>
    <h3>Routes serving station (${(d.routes_by_station ?? []).length})</h3>
    <div id="routes-station-chips"></div>
    <h3>Routes serving group (${(d.routes_by_group ?? []).length})</h3>
    <div id="routes-group-chips"></div>
  `);
  document.getElementById('group-chips').append(
    ...d.group_members.map((m) => {
      const el = document.createElement('span');
      el.className = 'chip-route';
      el.textContent = `站 ${m.station_id}`;
      el.title = m.station_name;
      return el;
    })
  );
  for (const [id, routes] of [
    ['routes-stop-chips', d.routes_by_stop],
    ['routes-station-chips', d.routes_by_station],
    ['routes-group-chips', d.routes_by_group],
  ]) {
    document.getElementById(id).append(routeChips(routes, onPickRoute));
  }
}
