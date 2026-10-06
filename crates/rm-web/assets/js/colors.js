// QA thresholds (mirror rm-verify semantics) and color helpers.

export const SNAP_GREEN = 15;   // m, p50
export const SNAP_AMBER = 40;   // m, p50

export function snapColor(p50) {
  if (p50 == null) return '#9ca3af';       // no data
  if (p50 < SNAP_GREEN) return '#2e9e44';
  if (p50 < SNAP_AMBER) return '#d19a00';
  return '#cf3e3e';
}

// path-vs-polyline deviation p95 thresholds (mirror rm-verify DEV_P95_WARN_M)
export const DEV_WARN = 50;   // m, flagged
export const DEV_BAD = 100;   // m, serious

export function devColor(p95) {
  if (p95 == null) return '#9ca3af';       // no data
  if (p95 < DEV_WARN) return '#2e9e44';
  if (p95 < DEV_BAD) return '#d19a00';
  return '#cf3e3e';
}

// matched_ratio thresholds: <0.9 amber, <0.5 red
export function matchedBadge(ratio) {
  if (ratio == null) return { cls: 'nodata', text: 'no data' };
  if (ratio >= 0.9) return { cls: 'good', text: `${Math.round(ratio * 100)}%` };
  if (ratio >= 0.5) return { cls: 'warn', text: `${Math.round(ratio * 100)}%` };
  return { cls: 'bad', text: `${Math.round(ratio * 100)}%` };
}

// Shared-corridor heat for segments (route_count).
export function routeCountColor(n) {
  if (n <= 1) return '#7da7d9';
  if (n <= 3) return '#d19a00';
  return '#cf3e3e';
}

export const METHOD_COLORS = {
  PROJECTED: '#2e9e44',
  ENDPOINT: '#7da7d9',
  OUTLIER: '#cf3e3e',
  NO_PATH: '#9ca3af',
};
