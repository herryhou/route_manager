// Fetch helpers — one per endpoint, all returning parsed JSON.
// Throws Error(`HTTP ${status}: ${body}`) on non-2xx.

export async function fetchJson(path, { signal } = {}) {
  const res = await fetch(path, { signal });
  if (!res.ok) {
    let msg = '';
    try { msg = (await res.json()).error ?? ''; } catch { /* not json */ }
    throw new Error(`HTTP ${res.status}: ${msg || res.statusText}`);
  }
  return res.json();
}

export const stats = () => fetchJson('/api/stats');
export const routes = (q = '') => fetchJson(`/api/routes?q=${encodeURIComponent(q)}`);
export const routeDetail = (id) => fetchJson(`/api/routes/${id}`);
export const routePath = (id, dir) => fetchJson(`/api/routes/${id}/directions/${dir}/path`);
export const routeStops = (id, dir) => fetchJson(`/api/routes/${id}/directions/${dir}/stops`);
export const routeWays = (id, dir) => fetchJson(`/api/routes/${id}/directions/${dir}/ways`);
export const routeRaw = (id, dir) => fetchJson(`/api/routes/${id}/directions/${dir}/raw`);
export const stopDetail = (id) => fetchJson(`/api/stops/${id}`);
export const stationDetail = (id) => fetchJson(`/api/stations/${id}`);
export const stopsBbox = (bbox, kind) =>
  fetchJson(`/api/stops?bbox=${bbox}&kind=${kind}`);
export const stopsSearch = (q) => fetchJson(`/api/stops?q=${encodeURIComponent(q)}`);
export const stationsSearch = (q) => fetchJson(`/api/stations?q=${encodeURIComponent(q)}`);
export const stationNearest = (lat, lon, maxM) =>
  fetchJson(`/api/stations/nearest?lat=${lat}&lon=${lon}&max_m=${maxM}`);
export const outliers = (minSnap = 50) => fetchJson(`/api/qa/outliers?min_snap=${minSnap}`);
export const matchDevs = (minDev = 50) => fetchJson(`/api/qa/matchdev?min_dev=${minDev}`);

// --- edit mode (phase 1) ---
export const editToken = () => sessionStorage.getItem('rm-edit-token') ?? '';

async function editFetch(path, body) {
  const res = await fetch(path, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${editToken()}` },
    body: JSON.stringify(body),
  });
  if (!res.ok) {
    let msg = '';
    try { msg = (await res.json()).error ?? ''; } catch { /* not json */ }
    throw new Error(`HTTP ${res.status}: ${msg || res.statusText}`);
  }
  return res.json();
}

export const editStop = (svi, body) => editFetch(`/api/edit/stop/${svi}`, body);
export const regroupStop = (svi, body) => editFetch(`/api/edit/stop/${svi}/regroup`, body);
export const undoEdit = (id, note) => editFetch(`/api/edit/undo/${id}`, { note });
export const editLog = (entity, id) => fetchJson(`/api/edit/log?entity=${entity}&entity_id=${id}`);
export const renameStation = (svi, body) => editFetch(`/api/edit/station/${svi}`, body);
export const moveStation = (svi, body) => editFetch(`/api/edit/station/${svi}/move`, body);
export const editPolylineLine = (xno, dir, body) =>
  editFetch(`/api/edit/polyline/${encodeURIComponent(xno)}/${dir}/line`, body);
export const refreshStatus = () => fetchJson('/api/edit/refresh');
export const refreshApply = () => editFetch('/api/edit/refresh', {});
