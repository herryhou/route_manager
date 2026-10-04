//! Pure geometry helpers for stop projection and geometry cutting.
//!
//! All functions are side-effect free and operate on decoded paths
//! (consecutive-duplicate points already removed by the decoder).

use rm_core::LatLng;

/// Google reference vector: (38.5,-120.2) (40.7,-120.95) (43.252,-126.453)
pub const REF_PATH_ENC: &str = "_p~iF~ps|U_ulLnnqC_mqNvxq`@";

/// Cumulative arc length in metres; `cum[i]` = length of path[..=i].
/// Returns `vec![0.0]` for a single-point path, empty for empty.
pub fn cumulative_m(path: &[LatLng]) -> Vec<f64> {
    let mut cum = Vec::with_capacity(path.len());
    let mut acc = 0.0;
    for (i, p) in path.iter().enumerate() {
        if i > 0 {
            acc += rm_core::geo::haversine_m(path[i - 1], *p);
        }
        cum.push(acc);
    }
    cum
}

/// One stop projected onto the path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projection {
    pub chainage_m: f64,
    pub snap_dist_m: f64,
    pub projected: LatLng,
}

/// Project `p` onto the nearest path segment (point-to-segment over all
/// segments, never endpoint-only). Returns chainage along the path and the
/// perpendicular snap distance. Callers handle paths with < 2 points
/// (method ENDPOINT) before reaching this.
pub fn project_point(path: &[LatLng], cum: &[f64], p: &LatLng) -> Projection {
    let mut best: Option<Projection> = None;
    for i in 0..path.len().saturating_sub(1) {
        let (proj, dist) = rm_core::geo::point_to_segment_m(*p, path[i], path[i + 1]);
        if best.as_ref().map(|b| dist < b.snap_dist_m).unwrap_or(true) {
            let along = rm_core::geo::haversine_m(path[i], proj);
            best = Some(Projection {
                chainage_m: cum[i] + along,
                snap_dist_m: dist,
                projected: proj,
            });
        }
    }
    best.unwrap_or(Projection {
        chainage_m: 0.0,
        snap_dist_m: 0.0,
        projected: *p,
    })
}

/// Clamp chainages to be non-decreasing (running max). Returns the clamped
/// values and a flag per stop (true = was clamped to an earlier stop's
/// chainage).
pub fn clamp_monotonic(chainages: &[f64]) -> (Vec<f64>, Vec<bool>) {
    let mut out = Vec::with_capacity(chainages.len());
    let mut flags = Vec::with_capacity(chainages.len());
    let mut max = f64::NEG_INFINITY;
    for &c in chainages {
        let clamped = c < max;
        let v = if clamped { max } else { c };
        if !clamped {
            max = c;
        }
        out.push(v);
        flags.push(clamped);
    }
    (out, flags)
}

/// Extra snap distance tolerated beyond the best when gathering side
/// alternatives for sequence-aware selection: a loop-closure pole sits
/// within a few metres of both the first and last path leg, and the two
/// candidate snaps can differ by ~10 m.
pub const CAND_SLACK_M: f64 = 30.0;

/// Snap distance weight in sequence selection. A candidate a few metres
/// nearer the pole must not outvote a candidate hundreds of metres closer
/// in chainage order, but a genuinely nearer pole (real parallel corridor)
/// must: 10x balances the two.
pub const SNAP_WEIGHT: f64 = 10.0;

/// All side-alternative projections of `p` onto the path: every contiguous
/// run of path segments whose snap distance is within `slack_m` of the best
/// contributes its closest member. One entry per run, sorted by chainage —
/// a pole straddling a loop closure yields the start-side and end-side
/// candidate; a pole on a plain linear path yields exactly one.
pub fn project_candidates(
    path: &[LatLng],
    cum: &[f64],
    p: &LatLng,
    slack_m: f64,
) -> Vec<Projection> {
    let n = path.len().saturating_sub(1);
    if n == 0 {
        return vec![Projection {
            chainage_m: 0.0,
            snap_dist_m: 0.0,
            projected: *p,
        }];
    }
    let mut dists = vec![0.0f64; n];
    let mut projs = Vec::with_capacity(n);
    for i in 0..n {
        let (proj, dist) = rm_core::geo::point_to_segment_m(*p, path[i], path[i + 1]);
        dists[i] = dist;
        projs.push(Projection {
            chainage_m: cum[i] + rm_core::geo::haversine_m(path[i], proj),
            snap_dist_m: dist,
            projected: proj,
        });
    }
    let best = dists.iter().copied().fold(f64::INFINITY, f64::min);
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if dists[i] > best + slack_m {
            i += 1;
            continue;
        }
        // contiguous run of near-best segments: keep its closest member
        let mut j = i + 1;
        let mut k = i;
        while j < n && dists[j] <= best + slack_m {
            if dists[j] < dists[k] {
                k = j;
            }
            j += 1;
        }
        out.push(projs[k]);
        i = j;
    }
    out.sort_by(|a, b| a.chainage_m.total_cmp(&b.chainage_m));
    out
}

/// Pick one projection per stop (candidates from [`project_candidates`])
/// minimizing `Σ SNAP_WEIGHT·snap + |Δchainage|` between consecutive stops
/// — a Viterbi pass over per-stop alternatives. This is what keeps
/// loop-closure poles on the side their stop order implies:
/// nearest-segment alone snaps stop 1 of a circular route onto the last leg
/// of the path, and the monotonic clamp then pins every following stop to
/// that chainage (the 661 collapse).
pub fn select_sequence(cands: &[Vec<Projection>]) -> Vec<Projection> {
    let n = cands.len();
    if n == 0 {
        return Vec::new();
    }
    let mut cost: Vec<f64> = cands[0]
        .iter()
        .map(|c| SNAP_WEIGHT * c.snap_dist_m)
        .collect();
    let mut links: Vec<Vec<usize>> = Vec::with_capacity(n - 1);
    for i in 1..n {
        let (prev, cur) = (&cands[i - 1], &cands[i]);
        let mut next = vec![f64::INFINITY; cur.len()];
        let mut link = vec![0usize; cur.len()];
        for (k, ck) in cur.iter().enumerate() {
            for (j, (pj, pc)) in prev.iter().zip(cost.iter()).enumerate() {
                let c = pc + (ck.chainage_m - pj.chainage_m).abs();
                if c < next[k] {
                    next[k] = c;
                    link[k] = j;
                }
            }
        }
        for (k, ck) in cur.iter().enumerate() {
            next[k] += SNAP_WEIGHT * ck.snap_dist_m;
        }
        links.push(link);
        cost = next;
    }
    let mut k = cost
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.total_cmp(b.1))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let mut out = vec![cands[n - 1][k]];
    for i in (1..n).rev() {
        k = links[i - 1][k];
        out.push(cands[i - 1][k]);
    }
    out.reverse();
    out
}

/// Interpolate the path point at chainage `x` (clamped to [0, total]).
fn point_at(path: &[LatLng], cum: &[f64], x: f64) -> LatLng {
    let last = path.len() - 1;
    for i in 0..last {
        if x <= cum[i + 1] || i + 1 == last {
            let seg = cum[i + 1] - cum[i];
            let t = if seg > 0.0 {
                ((x - cum[i]) / seg).clamp(0.0, 1.0)
            } else {
                0.0
            };
            return LatLng {
                lat: path[i].lat + t * (path[i + 1].lat - path[i].lat),
                lon: path[i].lon + t * (path[i + 1].lon - path[i].lon),
            };
        }
    }
    unreachable!("point_at requires a non-empty path")
}

/// Sub-path between chainages `a_m` and `b_m` (a <= b), endpoints
/// interpolated. Degenerate zero-width cut returns `[cut_point, cut_point]`.
pub fn cut_subpath(path: &[LatLng], cum: &[f64], a_m: f64, b_m: f64) -> (Vec<LatLng>, f64) {
    let total = cum.last().copied().unwrap_or(0.0);
    let a = a_m.clamp(0.0, total);
    let b = b_m.clamp(0.0, total);
    if b <= a {
        let p = point_at(path, cum, a);
        return (vec![p, p], 0.0);
    }
    let mut out = vec![point_at(path, cum, a)];
    for i in 1..path.len() {
        if cum[i] > a && cum[i] < b {
            out.push(path[i]);
        }
    }
    out.push(point_at(path, cum, b));
    (out, b - a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rm_core::polyline;

    fn ref_path() -> Vec<LatLng> {
        polyline::decode(REF_PATH_ENC).unwrap()
    }

    #[test]
    fn exact_path_points_project_with_zero_snap() {
        let p = ref_path();
        let cum = cumulative_m(&p);
        for i in 0..p.len() {
            let pr = project_point(&p, &cum, &p[i]);
            assert!(
                (pr.chainage_m - cum[i]).abs() < 1e-6,
                "stop {i}: chainage {} want {}",
                pr.chainage_m,
                cum[i]
            );
            assert!(pr.snap_dist_m < 1e-6, "stop {i}: snap {}", pr.snap_dist_m);
        }
    }

    #[test]
    fn offset_point_snaps_to_nearest_segment_with_perpendicular_distance() {
        let p = ref_path();
        let cum = cumulative_m(&p);
        // 0.05 deg due west of p[0]; leg 0 heads NW, so the offset decomposes
        // into ~1.1 km forward + ~4.2 km perpendicular
        let q = LatLng {
            lat: 38.5,
            lon: -120.25,
        };
        let pr = project_point(&p, &cum, &q);
        assert!(
            pr.chainage_m < 2000.0,
            "mostly lateral => chainage {}",
            pr.chainage_m
        );
        assert!(
            (4100.0..=4300.0).contains(&pr.snap_dist_m),
            "snap {}",
            pr.snap_dist_m
        );
    }

    #[test]
    fn clamp_monotonic_running_max_with_flags() {
        let (v, f) = clamp_monotonic(&[10.0, 5.0, 7.0, 12.0, 3.0]);
        assert_eq!(v, vec![10.0, 10.0, 10.0, 12.0, 12.0]);
        assert_eq!(f, vec![false, true, true, false, true]);
    }

    #[test]
    fn cut_full_range_returns_whole_path_and_total_length() {
        let p = ref_path();
        let cum = cumulative_m(&p);
        let total = *cum.last().unwrap();
        let (sub, len) = cut_subpath(&p, &cum, 0.0, total);
        assert_eq!(sub.len(), p.len());
        assert!((len - total).abs() < 1e-6);
    }

    #[test]
    fn cut_partial_interpolates_endpoints() {
        let p = ref_path();
        let cum = cumulative_m(&p);
        let a = cum[1];
        let b = (cum[1] + cum[2]) / 2.0;
        let (sub, len) = cut_subpath(&p, &cum, a, b);
        assert_eq!(
            sub.len(),
            2,
            "no interior vertex between cum[1] and mid leg2"
        );
        assert!((sub[0].lat - p[1].lat).abs() < 1e-9);
        assert!((sub[0].lon - p[1].lon).abs() < 1e-9);
        let want = (cum[2] - cum[1]) / 2.0;
        assert!((len - want).abs() / want < 1e-4, "len {len} want {want}");
        // interpolated last point lies on leg2
        let (_, d) = rm_core::geo::point_to_segment_m(sub[1], p[1], p[2]);
        assert!(d < 1.0, "interpolated point off leg2 by {d}");
    }

    #[test]
    fn cut_zero_width_returns_degenerate_pair() {
        let p = ref_path();
        let cum = cumulative_m(&p);
        let (sub, len) = cut_subpath(&p, &cum, cum[1], cum[1]);
        assert_eq!(sub.len(), 2);
        assert!(len.abs() < 1e-9);
    }

    fn proj(chainage_m: f64, snap_dist_m: f64) -> Projection {
        Projection {
            chainage_m,
            snap_dist_m,
            projected: LatLng::new(0.0, 0.0),
        }
    }

    #[test]
    fn loop_closure_picks_sequence_consistent_side() {
        // near-square loop starting and ending at the same corner (closure
        // gap ~10 m). The first stop's pole sits nearer the END side, so
        // nearest-segment alone snaps it to the final leg — the 661-style
        // collapse. Stop order must keep it at chainage ~0 and put the
        // revisit (stop 4, same pole) on the end side instead.
        let path = vec![
            LatLng::new(24.1400, 120.6857),  // A start
            LatLng::new(24.1400, 120.6867),  // B east
            LatLng::new(24.1410, 120.6867),  // C north
            LatLng::new(24.1410, 120.6857),  // D west
            LatLng::new(24.14009, 120.68571), // A' near closure
        ];
        let cum = cumulative_m(&path);
        let total = *cum.last().unwrap();
        // ~3.4 m from A' vs ~6.8 m from A: end side is the plain nearest
        let start_pole = LatLng::new(24.14006, 120.685715);
        let stops = [
            start_pole,
            LatLng::new(24.1400, 120.6862),   // mid A-B
            LatLng::new(24.1405, 120.6867),   // mid B-C
            start_pole,                        // revisit at closure
            path[4],                           // A' itself
        ];
        let cands: Vec<Vec<Projection>> = stops
            .iter()
            .map(|s| project_candidates(&path, &cum, s, CAND_SLACK_M))
            .collect();
        let sel = select_sequence(&cands);
        assert!(
            sel[0].chainage_m < 30.0,
            "first stop must stay start side, got {}",
            sel[0].chainage_m
        );
        assert!(
            sel[3].chainage_m > total - 30.0,
            "revisit must land end side, got {} of {total}",
            sel[3].chainage_m
        );
        assert!(sel[1].chainage_m > sel[0].chainage_m);
        assert!(sel[2].chainage_m > sel[1].chainage_m);
        assert!(sel[4].chainage_m >= sel[3].chainage_m - 1e-6);
    }

    #[test]
    fn sequence_selection_penalizes_distant_snap() {
        // candidate 25 m off the pole but zero chainage jump must lose to
        // the 2 m snap 200 m ahead: snap term outweighs the jump
        let cands = vec![vec![proj(100.0, 2.0)], vec![proj(100.0, 25.0), proj(300.0, 2.0)]];
        let sel = select_sequence(&cands);
        assert!((sel[1].chainage_m - 300.0).abs() < 1e-9);
    }

    #[test]
    fn single_candidate_per_stop_passes_through() {
        let cands = vec![vec![proj(0.0, 1.0)], vec![proj(100.0, 2.0)], vec![proj(250.0, 1.5)]];
        let sel = select_sequence(&cands);
        assert_eq!(sel.iter().map(|p| p.chainage_m as i64).collect::<Vec<_>>(), vec![0, 100, 250]);
    }
}
