//! Segment identity: blake3 over the station ext_uid pair plus the
//! corridor key.
//!
//! `segment_id = blake3_128("tcg:node:{A} | tcg:node:{B} | {corridor_key}")`
//! `pair_key   = blake3_128("tcg:node:{A} | tcg:node:{B}")`
//!
//! A station pair may carry genuinely different corridors (e.g. an oneway
//! loop vs a shortcut); within a pair the registry clusters cuts by length
//! and each cluster is its own segment, keyed by its median length bucket
//! (`corridor_key`, a decimal string like "600"). Routes that travel the
//! same corridor share the id even when their matched way lists differ
//! (dual carriageways, detours); the per-route way list lives on the
//! traversal (`way_sig`) and the segment stores the majority variant.
//! The reverse corridor hashes differently (mirrored identity).

/// Directed station-pair key: groups a pair's corridor segments for QA.
pub fn pair_key(a_ext_uid: &str, b_ext_uid: &str) -> String {
    rm_core::blake3_128_hex(format!("{a_ext_uid} | {b_ext_uid}").as_bytes())
}

/// Directed segment id from station ext_uids and the corridor key.
pub fn segment_id(a_ext_uid: &str, b_ext_uid: &str, corridor_key: &str) -> String {
    rm_core::blake3_128_hex(format!("{a_ext_uid} | {b_ext_uid} | {corridor_key}").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_inputs_hash_the_same() {
        assert_eq!(
            pair_key("tcg:node:44", "tcg:node:46"),
            pair_key("tcg:node:44", "tcg:node:46")
        );
    }

    #[test]
    fn reverse_corridor_hash_differs() {
        assert_ne!(
            pair_key("tcg:node:44", "tcg:node:46"),
            pair_key("tcg:node:46", "tcg:node:44"),
            "directed segments must not collide"
        );
    }

    #[test]
    fn pair_key_matches_spec_literal() {
        // freeze the preimage format against a hand-computed digest
        let expect = rm_core::blake3_128_hex(b"tcg:node:44 | tcg:node:46");
        assert_eq!(pair_key("tcg:node:44", "tcg:node:46"), expect);
    }

    #[test]
    fn segment_id_matches_spec_literal() {
        // freeze the preimage format against a hand-computed digest
        let expect = rm_core::blake3_128_hex(b"tcg:node:44 | tcg:node:46 | 600");
        assert_eq!(segment_id("tcg:node:44", "tcg:node:46", "600"), expect);
    }

    #[test]
    fn segment_id_differs_per_corridor() {
        assert_ne!(
            segment_id("tcg:node:44", "tcg:node:46", "390"),
            segment_id("tcg:node:44", "tcg:node:46", "600"),
            "corridor split segments must not collide"
        );
    }

    #[test]
    fn segment_id_differs_from_pair_key() {
        assert_ne!(
            segment_id("tcg:node:44", "tcg:node:46", "600"),
            pair_key("tcg:node:44", "tcg:node:46")
        );
    }
}
