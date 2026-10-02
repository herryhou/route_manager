//! Local-metric geo helpers over WGS84 (no proj dependency — Taichung spans
//! ~40 km, equirectangular/Haversine error is negligible at stop scale).

use crate::LatLng;

const EARTH_RADIUS_M: f64 = 6_371_008.8;

const DEG_TO_M: f64 = std::f64::consts::PI * EARTH_RADIUS_M / 180.0;

/// Great-circle distance in metres.
pub fn haversine_m(a: LatLng, b: LatLng) -> f64 {
    let (lat1, lat2) = (a.lat.to_radians(), b.lat.to_radians());
    let dlat = (b.lat - a.lat).to_radians();
    let dlon = (b.lon - a.lon).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().asin()
}

/// Projection of `p` onto segment `a`-`b`.
///
/// Returns the projected point and its distance (m) from `p`. Uses local
/// equirectangular scaling around the segment — accurate at city scale.
/// A degenerate (zero-length) segment projects onto its only point.
pub fn point_to_segment_m(p: LatLng, a: LatLng, b: LatLng) -> (LatLng, f64) {
    let cos_lat = a.lat.to_radians().cos();
    // Local metre frame anchored at `a`.
    let bx = (b.lon - a.lon) * DEG_TO_M * cos_lat;
    let by = (b.lat - a.lat) * DEG_TO_M;
    let px = (p.lon - a.lon) * DEG_TO_M * cos_lat;
    let py = (p.lat - a.lat) * DEG_TO_M;
    let len2 = bx * bx + by * by;
    let t = if len2 == 0.0 {
        0.0
    } else {
        ((px * bx + py * by) / len2).clamp(0.0, 1.0)
    };
    let (qx, qy) = (t * bx, t * by);
    let proj = LatLng::new(a.lat + qy / DEG_TO_M, a.lon + qx / (DEG_TO_M * cos_lat));
    let dist = ((px - qx).powi(2) + (py - qy).powi(2)).sqrt();
    (proj, dist)
}

/// Polyline arc length in metres between points (sum of haversine legs).
pub fn path_length_m(points: &[LatLng]) -> f64 {
    points.windows(2).map(|w| haversine_m(w[0], w[1])).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real data: 干城站 node 44 vs 46 — README says 28 m apart.
    const G44: LatLng = LatLng::new(24.140968322753906, 120.68576049804688);
    const G46: LatLng = LatLng::new(24.140811920166016, 120.68598175048828);

    #[test]
    fn haversine_matches_measured_gancheng_pair() {
        let d = haversine_m(G44, G46);
        assert!((25.0..=31.0).contains(&d), "got {d}");
    }

    #[test]
    fn haversine_is_symmetric_and_zero_for_same_point() {
        assert_eq!(haversine_m(G44, G44), 0.0);
        assert!((haversine_m(G44, G46) - haversine_m(G46, G44)).abs() < 1e-9);
    }

    #[test]
    fn projects_onto_interior_perpendicular() {
        // p is ~1 arc-second east of segment midpoint
        let a = LatLng::new(24.0, 120.0);
        let b = LatLng::new(24.0, 120.01); // ~1019 m east
        let p = LatLng::new(24.001, 120.005);
        let (proj, dist) = point_to_segment_m(p, a, b);
        assert!(
            (proj.lat - 24.0).abs() < 1e-9,
            "proj on segment lat, got {}",
            proj.lat
        );
        assert!((proj.lon - 120.005).abs() < 1e-6);
        // perpendicular distance ≈ 0.001° lat ≈ 111 m
        assert!((105.0..=117.0).contains(&dist), "got {dist}");
    }

    #[test]
    fn projects_to_endpoint_when_beyond_segment() {
        let a = LatLng::new(24.0, 120.0);
        let b = LatLng::new(24.0, 120.01);
        let p = LatLng::new(24.0, 120.02); // past b
        let (proj, dist) = point_to_segment_m(p, a, b);
        assert!((proj.lon - 120.01).abs() < 1e-9);
        // 0.01° lon at lat 24 ≈ 1019 m
        assert!((1000.0..=1040.0).contains(&dist), "got {dist}");
    }

    #[test]
    fn degenerate_segment_projects_onto_its_point() {
        // ~93 m north of G44 — stop-scale, where equirect ≈ great-circle
        let p = LatLng::new(24.1418, 120.6859);
        let (proj, dist) = point_to_segment_m(p, G44, G44);
        assert_eq!(proj, G44);
        assert!((dist - haversine_m(p, G44)).abs() / haversine_m(p, G44) < 1e-4);
    }

    #[test]
    fn path_length_sums_legs() {
        let pts = [
            LatLng::new(24.0, 120.0),
            LatLng::new(24.0, 120.01),
            LatLng::new(24.0, 120.02),
        ];
        let total = path_length_m(&pts);
        // 0.01 deg lon at lat 24 = 1015.85 m; two legs = 2031.7 m
        assert!((2028.0..=2036.0).contains(&total), "got {total}");
        assert!(path_length_m(&[G44]).abs() < 1e-9);
    }
}
