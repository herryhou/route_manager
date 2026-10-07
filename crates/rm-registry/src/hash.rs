//! Segment identity: blake3 over the station ext_uid pair only.
//!
//! `segment_id = blake3_128("tcg:node:{A} | tcg:node:{B}")`
//! One directed segment per station pair: routes that travel between the
//! same stops share the id even when their matched way lists differ
//! (dual carriageways, detours); the per-route way list lives on the
//! traversal (`way_sig`) and the segment stores the majority variant.
//! The reverse corridor hashes differently (mirrored identity).

/// Directed segment id from station ext_uids.
pub fn segment_id(a_ext_uid: &str, b_ext_uid: &str) -> String {
    rm_core::blake3_128_hex(format!("{a_ext_uid} | {b_ext_uid}").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_inputs_hash_the_same() {
        assert_eq!(
            segment_id("tcg:node:44", "tcg:node:46"),
            segment_id("tcg:node:44", "tcg:node:46")
        );
    }

    #[test]
    fn reverse_corridor_hash_differs() {
        assert_ne!(
            segment_id("tcg:node:44", "tcg:node:46"),
            segment_id("tcg:node:46", "tcg:node:44"),
            "directed segments must not collide"
        );
    }

    #[test]
    fn format_matches_spec_literal() {
        // freeze the preimage format against a hand-computed digest
        let expect = rm_core::blake3_128_hex(b"tcg:node:44 | tcg:node:46");
        assert_eq!(segment_id("tcg:node:44", "tcg:node:46"), expect);
    }
}
