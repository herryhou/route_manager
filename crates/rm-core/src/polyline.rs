//! Google encoded polyline algorithm (precision 1e-5).
//!
//! Taichung ebus `routePoint.go`/`.back` use this format.

use crate::LatLng;
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum PolylineError {
    #[error("truncated varint at byte {0}")]
    TruncatedVarint(usize),
    #[error("invalid continuation byte {0}")]
    InvalidByte(usize),
}

/// Encode points, quantizing coordinates to 1e-5 (the format's precision).
pub fn encode(points: &[LatLng]) -> String {
    let mut s = String::with_capacity(points.len() * 8);
    let (mut plat, mut plon) = (0i64, 0i64);
    for p in points {
        let lat = (p.lat * 1e5).round() as i64;
        let lon = (p.lon * 1e5).round() as i64;
        write_varint(zigzag(lat - plat), &mut s);
        write_varint(zigzag(lon - plon), &mut s);
        plat = lat;
        plon = lon;
    }
    s
}

/// Decode an encoded polyline. Consecutive duplicate points are dropped —
/// raw captures contain 988 of them and they break chainage monotonicity.
pub fn decode(s: &str) -> Result<Vec<LatLng>, PolylineError> {
    let bytes = s.as_bytes();
    let mut pts = Vec::new();
    let (mut lat, mut lon) = (0i64, 0i64);
    let mut i = 0;
    while i < bytes.len() {
        let (dlat, next) = read_varint(bytes, i)?;
        let (dlon, next) = read_varint(bytes, next)?;
        i = next;
        lat += dlat;
        lon += dlon;
        let p = LatLng::new(lat as f64 / 1e5, lon as f64 / 1e5);
        if pts.last() != Some(&p) {
            pts.push(p);
        }
    }
    Ok(pts)
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn write_varint(mut v: u64, s: &mut String) {
    loop {
        let mut chunk = (v & 0x1f) as u8;
        v >>= 5;
        if v > 0 {
            chunk |= 0x20; // continuation bit
        }
        s.push((chunk + 63) as char);
        if v == 0 {
            break;
        }
    }
}

/// Read one 5-bit-group varint starting at `i`. Returns (value, next index).
fn read_varint(b: &[u8], mut i: usize) -> Result<(i64, usize), PolylineError> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        let byte = *b.get(i).ok_or(PolylineError::TruncatedVarint(i))?;
        if !(63..=126).contains(&byte) {
            return Err(PolylineError::InvalidByte(i));
        }
        let six = byte - 63;
        result |= u64::from(six & 0x1f) << shift;
        shift += 5;
        i += 1;
        if six < 32 {
            break;
        }
    }
    Ok((unzigzag(result), i))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Google's documented reference vector.
    #[test]
    fn encodes_google_reference_vector() {
        let pts = [
            LatLng::new(38.5, -120.2),
            LatLng::new(40.7, -120.95),
            LatLng::new(43.252, -126.453),
        ];
        assert_eq!(encode(&pts), "_p~iF~ps|U_ulLnnqC_mqNvxq`@");
    }

    #[test]
    fn rounds_beyond_1e5() {
        // 38.123456789 -> 38.12346 ; matches Google reference behaviour and
        // keeps decode/encode grid-stable
        assert_eq!(
            encode(&[LatLng::new(38.123456789, -120.2)]),
            encode(&[LatLng::new(38.12346, -120.2)])
        );
    }

    /// Real capture: assets/Taichung/raw/route_1010.json routePoint.go.
    /// Round trip must be byte-identical (this string has no consecutive dupes).
    const REAL_GO: &str = "yaurCo~k_VoB`@cAmD`MaHr@`CdAdFb@|A\\vAb@`BlAvEpAvELn@FTDPBJ@JBH@D?@DNBFNGvAg@oIsZvAm@bFgEjIcIoAcCxBeA`B_@zCWvKSjIU`@?|@EtEMxMWrf@yDtAEj@iAhAo@bAYjBe@lBk@rIaC|@Up@OVKDHBDDXRfA";

    #[test]
    fn roundtrips_real_capture_byte_identical() {
        let pts = decode(REAL_GO).unwrap();
        assert!(pts.len() > 10);
        assert_eq!(encode(&pts), REAL_GO);
    }

    #[test]
    fn decodes_google_reference_vector() {
        let pts = decode("_p~iF~ps|U_ulLnnqC_mqNvxq`@").unwrap();
        assert_eq!(pts.len(), 3);
        assert!((pts[0].lat - 38.5).abs() < 1e-5);
        assert!((pts[0].lon - (-120.2)).abs() < 1e-5);
        assert!((pts[2].lat - 43.252).abs() < 1e-5);
        assert!((pts[2].lon - (-126.453)).abs() < 1e-5);
    }

    #[test]
    fn decode_dedups_consecutive_duplicates() {
        // (0,0) (0,0) (1,0): delta encoding of the dupe is a zero varint pair.
        let pts = vec![
            LatLng::new(0.0, 0.0),
            LatLng::new(0.0, 0.0),
            LatLng::new(1.0, 0.0),
        ];
        let enc = encode(&pts);
        assert_eq!(
            decode(&enc).unwrap(),
            vec![LatLng::new(0.0, 0.0), LatLng::new(1.0, 0.0)]
        );
    }

    #[test]
    fn empty_input_roundtrip() {
        assert_eq!(encode(&[]), "");
        assert_eq!(decode("").unwrap(), Vec::<LatLng>::new());
    }

    #[test]
    fn truncated_varint_is_an_error() {
        // "_p~iF" decodes one point; chop a byte mid-varint.
        assert!(matches!(
            decode("_p~i"),
            Err(PolylineError::TruncatedVarint(_))
        ));
    }

    #[test]
    fn non_ascii_byte_is_an_error() {
        // Format bytes are ASCII 63..=126; 'Ā' encodes as 0xC4 0x80, both >126.
        assert!(matches!(
            decode("\u{100}"),
            Err(PolylineError::InvalidByte(_))
        ));
    }
}
