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
