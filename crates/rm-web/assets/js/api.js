// Fetch helpers — one per endpoint, all returning parsed JSON.
// Throws Error(`HTTP ${status}: ${body}`) on non-2xx.
//
// Response shapes: the authority is the serde struct beside each query fn
// in crates/rm-web/src/queries/*.rs; the typedefs below mirror them and
// are pinned by crates/rm-web/tests/shape.rs. One deliberate drift:
// /api/stations/nearest spells latitude/longitude, everything else lat/lon.

export async function fetchJson(path, { headers, signal } = {}) {
  const res = await fetch(path, { headers, signal });
  if (!res.ok) {
    let msg = '';
    try { msg = (await res.json()).error ?? ''; } catch { /* not json */ }
    throw new Error(`HTTP ${res.status}: ${msg || res.statusText}`);
  }
  return res.json();
}

/**
 * @typedef {Object} ServingRoute
 * @property {number} route_id
 * @property {string} route_code
 * @property {?string} subroute_name
 * @property {number} direction
 * @property {?number} bearing member pole heading (side-of-road pick)
 */
/**
 * @typedef {Object} GroupMember
 * @property {number} station_id
 * @property {string} station_name
 * @property {number} lat
 * @property {number} lon
 */
/**
 * @typedef {Object} DirectionRow
 * @property {number} route_id
 * @property {number} direction
 * @property {number} stops
 * @property {?number} matched_ratio
 * @property {?number} snap_dist_p50_m
 * @property {?number} snap_dist_p95_m
 * @property {?number} outliers
 */

/** @returns {Promise<any>} counts + match summary + latest batch (+ edit_enabled, handler-added) */
export const stats = () => fetchJson('/api/stats');
/** @returns {Promise<{routes: Array<{route_id: number, route_code: string, ext_uid: string, subroute_name: ?string, providers: string[], side_routes: string[], main_routes: string[], directions: DirectionRow[]}>}>} */
export const routes = (q = '') => fetchJson(`/api/routes?q=${encodeURIComponent(q)}`);
/** @returns {Promise<any>} 404 when unknown */
export const routeDetail = (id) => fetchJson(`/api/routes/${id}`);
/** @returns {Promise<any>} GeoJSON FeatureCollection; piece props: ordinal, length_m, geometry_method, from_seq, to_seq, from/to_station_id, from/to_station_name */
export const routePath = (id, dir) => fetchJson(`/api/routes/${id}/directions/${dir}/path`);
/** @returns {Promise<{stops: Array<{stop_sequence: number, source_order_no: ?number, stop_version_id: number, stop_code: string, stop_name: string, lat: number, lon: number, station_id: number, chainage_m: ?number, snap_dist_m: ?number, method: ?string, monotonic_clamped: ?number, projected: ?number[], bearing: ?number, route_codes: string[]}>}>} */
export const routeStops = (id, dir) => fetchJson(`/api/routes/${id}/directions/${dir}/stops`);
/** @returns {Promise<any>} GeoJSON FeatureCollection; way props: ordinal, osm_way_id, name, entry/exit_node_id, entry/exit_coords, cum_start_m, cum_end_m */
export const routeWays = (id, dir) => fetchJson(`/api/routes/${id}/directions/${dir}/ways`);
/** @returns {Promise<any>} staging compare: batch_id, path?, stops[], effective{points, source, updated_at} */
export const routeRaw = (id, dir) => fetchJson(`/api/routes/${id}/directions/${dir}/raw`);
/** @returns {Promise<any>} stop → station → group + bearings + serving routes + stale badges */
export const stopDetail = (id) => fetchJson(`/api/stops/${id}`);
/** @returns {Promise<any>} station → group + stops + serving routes + stale badges */
export const stationDetail = (id) => fetchJson(`/api/stations/${id}`);
/** @returns {Promise<{corridors: Array<{corridor_m: number, min_m: number, max_m: number, routes: Array<{route_id: number, route_code: string, subroute_name: ?string, direction: number, length_m: number}>}>}>} */
export const stationPairRoutes = (fromStation, toStation) =>
  fetchJson(`/api/station-pair/${fromStation}/${toStation}/routes`);
/** @returns {Promise<any>} GeoJSON FeatureCollection; props id, name, kind, bearing, + spread (station) or bearing_source (stop) — the other key absent */
export const stopsBbox = (bbox, kind) =>
  fetchJson(`/api/stops?bbox=${bbox}&kind=${kind}`);
/** @returns {Promise<{stops: Array<{stop_version_id: number, stop_code: string, stop_name: string, lat: number, lon: number}>}>} */
export const stopsSearch = (q) => fetchJson(`/api/stops?q=${encodeURIComponent(q)}`);
/** @returns {Promise<{stations: Array<{station_id: number, station_name: string, lat: number, lon: number}>}>} */
export const stationsSearch = (q) => fetchJson(`/api/stations?q=${encodeURIComponent(q)}`);
/** @returns {Promise<{station_id: number, station_name: string, latitude: number, longitude: number, distance_m: number}>} — note the full latitude/longitude spelling, unique to this endpoint */
export const stationNearest = (lat, lon, maxM) =>
  fetchJson(`/api/stations/nearest?lat=${lat}&lon=${lon}&max_m=${maxM}`);
/** @returns {Promise<{outliers: Array<{route_id: number, route_code: string, direction: number, stop_version_id: number, stop_name: string, stop_sequence: number, lat: number, lon: number, chainage_m: ?number, snap_dist_m: number, method: string, projected: ?number[]}>, count: number}>} */
export const outliers = (minSnap = 50) => fetchJson(`/api/qa/outliers?min_snap=${minSnap}`);
/** @returns {Promise<{directions: Array<{route_id: number, route_code: string, direction: number, stops: number, matched_ratio: ?number, dev_p50_m: number, dev_p95_m: number, dev_max_m: number, snap_dist_p95: ?number, gap_repairs: number, outliers: number}>, count: number}>} */
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
export const refreshStatus = () =>
  fetchJson('/api/edit/refresh', { headers: { authorization: `Bearer ${editToken()}` } });
export const refreshApply = () => editFetch('/api/edit/refresh', {});
