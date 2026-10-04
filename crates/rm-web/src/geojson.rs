//! Minimal GeoJSON builders. Coordinates are `[lon, lat]` (RFC 7946);
//! `rm_core::LatLng` is bus-domain (lat, lon) — convert with [`line_string`].

use rm_core::LatLng;
use serde_json::{json, Value};

/// `[[lon, lat], ...]` from bus-domain points.
pub fn coords(points: &[LatLng]) -> Vec<Value> {
    points.iter().map(|p| json!([p.lon, p.lat])).collect()
}

pub fn line_string(points: &[LatLng]) -> Value {
    json!({ "type": "LineString", "coordinates": coords(points) })
}

/// One LineString per piece; for ways that chain into disconnected strands.
pub fn multi_line_string(pieces: &[Vec<LatLng>]) -> Value {
    json!({
        "type": "MultiLineString",
        "coordinates": pieces.iter().map(|p| coords(p)).collect::<Vec<_>>()
    })
}

pub fn point(lon: f64, lat: f64) -> Value {
    json!({ "type": "Point", "coordinates": [lon, lat] })
}

pub fn feature(geometry: Value, properties: Value) -> Value {
    json!({ "type": "Feature", "geometry": geometry, "properties": properties })
}

pub fn feature_collection(features: Vec<Value>) -> Value {
    json!({ "type": "FeatureCollection", "features": features })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coords_are_lon_first() {
        let c = coords(&[LatLng {
            lat: 24.14,
            lon: 120.68,
        }]);
        let pair = c[0].as_array().unwrap();
        assert_eq!(pair[0].as_f64().unwrap(), 120.68, "lon first");
        assert_eq!(pair[1].as_f64().unwrap(), 24.14, "lat second");
    }
}
