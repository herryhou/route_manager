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
}
