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
export const segmentsBbox = (bbox, minRoutes = 0, geom = 'provider') =>
  fetchJson(`/api/segments?bbox=${bbox}&min_routes=${minRoutes}&geom=${geom}`);
export const segmentDetail = (id) => fetchJson(`/api/segments/${id}`);
export const stopsBbox = (bbox, kind) =>
  fetchJson(`/api/stops?bbox=${bbox}&kind=${kind}`);
export const stopsSearch = (q) => fetchJson(`/api/stops?q=${encodeURIComponent(q)}`);
export const stationsSearch = (q) => fetchJson(`/api/stations?q=${encodeURIComponent(q)}`);
export const outliers = (minSnap = 50) => fetchJson(`/api/qa/outliers?min_snap=${minSnap}`);
