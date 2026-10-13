// QA thresholds (mirror rm-verify semantics) and color helpers.

export const SNAP_GREEN = 15;   // m, p50
export const SNAP_AMBER = 40;   // m, p50

export function snapColor(p50) {
  if (p50 == null) return '#8a8880';       // no data
  if (p50 < SNAP_GREEN) return '#1e7a3c';
  if (p50 < SNAP_AMBER) return '#e8a013';
  return '#c0331b';
}

// path-vs-polyline deviation p95 thresholds (mirror rm-verify DEV_P95_WARN_M)
export const DEV_WARN = 50;   // m, flagged
export const DEV_BAD = 100;   // m, serious

export function devColor(p95) {
  if (p95 == null) return '#8a8880';       // no data
  if (p95 < DEV_WARN) return '#1e7a3c';
  if (p95 < DEV_BAD) return '#e8a013';
  return '#c0331b';
}

// matched_ratio thresholds: <0.9 amber, <0.5 red
export function matchedBadge(ratio) {
  if (ratio == null) return { cls: 'nodata', text: 'no data' };
  if (ratio >= 0.9) return { cls: 'good', text: `${Math.round(ratio * 100)}%` };
  if (ratio >= 0.5) return { cls: 'warn', text: `${Math.round(ratio * 100)}%` };
  return { cls: 'bad', text: `${Math.round(ratio * 100)}%` };
}

export const METHOD_COLORS = {
  PROJECTED: '#1e7a3c',
  ENDPOINT: '#7da7d9',
  OUTLIER: '#c0331b',
  NO_PATH: '#8a8880',
};

/// QA node spec for the 站序鏈: dot radius and hatch spacing grow with
/// snap metres, colour only reinforcing (green <15m, amber <40m, red past
/// NO_PATH/OUTLIER or 40m). --nd = node diameter px, --nh = hatch period px
/// (0 = solid, ok band). Long stops (null snap) take the amber-magnitude
/// default so an unknown stays visibly "not clean".
export function qaDot(s) {
  const snap = s.snap_dist_m;
  const bad = s.method === 'OUTLIER' || s.method === 'NO_PATH' || (snap != null && snap > SNAP_AMBER);
  const warn = !bad && snap != null && snap > SNAP_GREEN;
  const scale = Math.min(snap ?? SNAP_AMBER, 120) / 120;
  const nd = 8 + scale * 5;              // 8px .. 13px node incl. ring
  const nh = bad ? 2 : warn ? 3.5 - snap / 40 : 0;
  return {
    cls: bad ? 'q-bad' : warn ? 'q-warn' : 'q-ok',
    nd: nd.toFixed(1),
    nh: Math.max(nh, 1.6).toFixed(2),
    label: bad && s.method === 'NO_PATH' && snap == null ? '未比對' : `${snap?.toFixed(0) ?? '—'}m`,
  };
}
