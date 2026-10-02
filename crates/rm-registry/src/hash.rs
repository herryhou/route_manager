//! Segment identity: blake3 over station ext_uids + travel-order ways.
//!
//! `segment_id = blake3_128("tcg:node:{A} | tcg:node:{B} | w1|…|wn")`
//! The reverse corridor hashes differently (mirrored identity): same
//! stations swapped and ways reversed produce their own id.

/// Directed segment id from station ext_uids and travel-order way ids.
pub fn segment_id(a_ext_uid: &str, b_ext_uid: &str, way_ids: &[i64]) -> String {
    let ways = way_ids
        .iter()
        .map(|w| w.to_string())
        .collect::<Vec<_>>()
        .join("|");
    rm_core::blake3_128_hex(format!("{a_ext_uid} | {b_ext_uid} | {ways}").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_inputs_hash_the_same() {
        assert_eq!(
            segment_id("tcg:node:44", "tcg:node:46", &[100, 200]),
            segment_id("tcg:node:44", "tcg:node:46", &[100, 200])
        );
    }

    #[test]
    fn reverse_corridor_hash_differs() {
        let fwd = segment_id("tcg:node:44", "tcg:node:46", &[100, 200]);
        let rev = segment_id("tcg:node:46", "tcg:node:44", &[200, 100]);
        assert_ne!(fwd, rev, "directed segments must not collide");
    }

    #[test]
    fn different_ways_hash_differently() {
        assert_ne!(
            segment_id("tcg:node:44", "tcg:node:46", &[100]),
            segment_id("tcg:node:44", "tcg:node:46", &[101])
        );
    }

    #[test]
    fn format_matches_spec_literal() {
        // freeze the preimage format against a hand-computed digest
        let expect = rm_core::blake3_128_hex(b"tcg:node:44 | tcg:node:46 | 100|200");
        assert_eq!(
            segment_id("tcg:node:44", "tcg:node:46", &[100, 200]),
            expect
        );
    }
}
