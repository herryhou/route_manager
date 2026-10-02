//! rm-core: shared domain types, Google polyline codec, geo helpers, hashing.

pub mod geo;
pub mod polyline;

/// WGS84 latitude/longitude pair. Bus-domain order (lat, lon), matching the
/// source APIs; convert to geo-types `Coord` (x=lon, y=lat) only at the
/// geometry boundary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatLng {
    pub lat: f64,
    pub lon: f64,
}

impl LatLng {
    pub const fn new(lat: f64, lon: f64) -> Self {
        Self { lat, lon }
    }
}

/// 128-bit blake3 hash, hex-encoded — segment identity scheme.
pub fn blake3_128_hex(input: &[u8]) -> String {
    let mut out = [0u8; 16];
    blake3::Hasher::new()
        .update(input)
        .finalize_xof()
        .fill(&mut out);
    let mut s = String::with_capacity(32);
    for b in out {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blake3_128_hex_is_stable_and_short() {
        let h = blake3_128_hex(b"tcg:node:44 | tcg:node:46 | 123");
        assert_eq!(h.len(), 32);
        assert_eq!(h, blake3_128_hex(b"tcg:node:44 | tcg:node:46 | 123"));
        assert_ne!(h, blake3_128_hex(b"tcg:node:46 | tcg:node:44 | 123"));
    }
}
