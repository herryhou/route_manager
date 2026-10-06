//! Highway whitelist + oneway tag interpretation (pure, unit-tested).

/// Road classes buses can use; links, bus lanes and the legacy
/// bus_guideway included. `busway` is the live tag for median bus lanes
/// (仁愛路/信義路/新生南路 counterflow lanes in Taipei); without it every
/// eastbound 仁愛路 match detours through back alleys. `living_street`
/// and `road` cover real drivable streets outside the main class system
/// (shared-space lanes, unclassified-in-progress roads); `track` stays
/// out — farm paths buses cannot use would only add A* noise.
pub const HIGHWAY_WHITELIST: &[&str] = &[
    "motorway",
    "motorway_link",
    "trunk",
    "trunk_link",
    "primary",
    "primary_link",
    "secondary",
    "secondary_link",
    "tertiary",
    "tertiary_link",
    "unclassified",
    "residential",
    "living_street",
    "road",
    "service",
    "bus_guideway",
    "busway",
];

/// True when the way's `highway` tag is whitelisted (None = untagged).
pub fn highway_allowed(highway: Option<&str>) -> bool {
    highway.is_some_and(|h| HIGHWAY_WHITELIST.contains(&h))
}

/// oneway tag -> 1 (forward), -1 (reverse), 0 (no / unknown).
pub fn oneway_value(v: &str) -> i64 {
    match v {
        "yes" | "true" | "1" => 1,
        "-1" | "reverse" => -1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_accepts_plan_classes_and_rejects_rest() {
        for h in [
            "motorway",
            "motorway_link",
            "trunk_link",
            "primary",
            "tertiary_link",
            "unclassified",
            "residential",
            "service",
            "bus_guideway",
            "busway",
            "living_street",
            "road",
        ] {
            assert!(highway_allowed(Some(h)), "{h} should pass");
        }
        for h in [
            "footway",
            "cycleway",
            "path",
            "pedestrian",
            "track",
            "steps",
            "proposed",
        ] {
            assert!(!highway_allowed(Some(h)), "{h} should fail");
        }
        assert!(!highway_allowed(None));
    }

    #[test]
    fn oneway_tag_variants() {
        assert_eq!(oneway_value("yes"), 1);
        assert_eq!(oneway_value("true"), 1);
        assert_eq!(oneway_value("1"), 1);
        assert_eq!(oneway_value("-1"), -1);
        assert_eq!(oneway_value("reverse"), -1);
        assert_eq!(oneway_value("no"), 0);
        assert_eq!(oneway_value("alternating"), 0);
        assert_eq!(oneway_value(""), 0);
    }
}
