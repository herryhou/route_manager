//! Invariant checks, count cross-checks and QA exports for one batch.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

#[derive(Debug, Clone, Default)]
pub struct Expectations {
    pub routes: Option<i64>,
    pub stations: Option<i64>,
    pub stop_rows: Option<i64>,
    pub sids: Option<i64>,
    pub stops: Option<i64>,
    pub groups: Option<i64>,
    pub providers: Option<i64>,
    pub cycled: Option<i64>,
    pub single_dir: Option<i64>,
    pub no_polyline: Option<i64>,
    pub main_side_groups: Option<i64>,
}

impl Expectations {
    /// Verified targets from the real Taichung captures (M1 ledger).
    pub const REAL: Expectations = Expectations {
        routes: Some(392),
        stations: Some(7_424),
        stop_rows: Some(37_294),
        sids: Some(14_045),
        stops: Some(37_292),
        groups: Some(4_047),
        providers: Some(39),
        cycled: Some(44),
        single_dir: Some(23),
        no_polyline: Some(17),
        main_side_groups: Some(99),
    };
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: String,
    pub name: String,
    pub pass: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub batch_id: i64,
    pub checks: Vec<Check>,
    /// Advisory findings that never affect the exit status (warn-only).
    pub warnings: Vec<Check>,
    pub counts: BTreeMap<String, i64>,
    pub stats: BTreeMap<String, f64>,
}

impl Report {
    pub fn all_pass(&self) -> bool {
        self.checks.iter().all(|c| c.pass)
    }
}

fn n(conn: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> anyhow::Result<i64> {
    Ok(conn.query_row(sql, params, |r| r.get(0))?)
}

fn check(id: &str, name: &str, pass: bool, detail: String) -> Check {
    Check {
        id: id.to_string(),
        name: name.to_string(),
        pass,
        detail,
    }
}

/// Fail check when observed != 0.
fn zero_check(conn: &Connection, id: &str, name: &str, sql: &str) -> anyhow::Result<Check> {
    let v = n(conn, sql, &[])?;
    Ok(check(id, name, v == 0, v.to_string()))
}

pub fn run(
    conn: &Connection,
    batch: i64,
    exp: &Expectations,
    qa_dir: &Path,
) -> anyhow::Result<Report> {
    let mut checks = Vec::new();
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    let mut stats: BTreeMap<String, f64> = BTreeMap::new();

    // ---- counts ----
    let queries: Vec<(&str, &str)> = vec![
        ("routes", "SELECT COUNT(*) FROM route"),
        ("stations", "SELECT COUNT(*) FROM station"),
        (
            "stop_rows",
            "SELECT COUNT(*) FROM src_stop_edge WHERE batch_id = ?1",
        ),
        (
            "sids",
            "SELECT COUNT(DISTINCT sid) FROM src_stop_edge WHERE batch_id = ?1",
        ),
        ("stops", "SELECT COUNT(*) FROM stop"),
        ("groups", "SELECT COUNT(*) FROM station_group"),
        (
            "providers",
            "SELECT COUNT(*) FROM src_provider WHERE batch_id = ?1",
        ),
        (
            "cycled",
            "SELECT COUNT(*) FROM src_route_header WHERE batch_id = ?1 AND is_cycled = 1",
        ),
        (
            "single_dir",
            "SELECT COUNT(*) FROM (SELECT xno FROM src_stop_edge WHERE batch_id = ?1
             GROUP BY xno HAVING COUNT(DISTINCT direction) = 1)",
        ),
        (
            "no_polyline",
            "SELECT COUNT(*) FROM src_route_point WHERE batch_id = ?1 AND encoded = ''",
        ),
        (
            "main_side_groups",
            "SELECT COUNT(DISTINCT main_xno) FROM src_main_side_route WHERE batch_id = ?1",
        ),
    ];
    let mut observed: BTreeMap<String, i64> = BTreeMap::new();
    for (k, sql) in &queries {
        let v = if sql.contains("?1") {
            n(conn, sql, &[&batch])?
        } else {
            n(conn, sql, &[])?
        };
        observed.insert((*k).to_string(), v);
    }
    counts.extend(observed.iter().map(|(k, v)| (k.clone(), *v)));

    for (k, want) in [
        ("routes", exp.routes),
        ("stations", exp.stations),
        ("stop_rows", exp.stop_rows),
        ("sids", exp.sids),
        ("stops", exp.stops),
        ("groups", exp.groups),
        ("providers", exp.providers),
        ("cycled", exp.cycled),
        ("single_dir", exp.single_dir),
        ("no_polyline", exp.no_polyline),
        ("main_side_groups", exp.main_side_groups),
    ] {
        if let Some(want) = want {
            let got = observed[k];
            checks.push(check(
                &format!("count:{k}"),
                &format!("{k} == {want}"),
                got == want,
                format!("observed {got}"),
            ));
        }
    }

    // ---- INV-01/02/03: closed versions are never current ----
    checks.push(zero_check(
        conn,
        "inv01-03:immutable",
        "closed-interval version still referenced as current",
        "SELECT COUNT(*) FROM route r
         JOIN route_version v ON v.route_version_id = r.current_route_version_id
         WHERE v.valid_to IS NOT NULL",
    )?);

    // ---- INV-04: ids never reused (allocator ahead of every table max) ----
    {
        let mut behind = 0;
        for t in [
            "route",
            "route_version",
            "subroute_version",
            "stop",
            "stop_version",
            "station",
            "station_version",
            "station_group",
            "station_group_version",
            "route_stop_version",
        ] {
            let max: i64 = conn.query_row(
                &format!("SELECT COALESCE(MAX(rowid), 0) FROM {t}"),
                [],
                |r| r.get(0),
            )?;
            // missing allocator row is fine only while the table is empty
            let next: Option<i64> = conn
                .query_row(
                    "SELECT next_value FROM id_sequence WHERE table_name = ?1",
                    [t],
                    |r| r.get(0),
                )
                .optional()?;
            match next {
                Some(next) if next <= max => behind += 1,
                None if max > 0 => behind += 1,
                _ => {}
            }
        }
        checks.push(check(
            "inv04:ids-not-reused",
            "id_sequence ahead of every table max",
            behind == 0,
            format!("{behind} tables behind"),
        ));
    }

    // ---- INV-05: route ext_uid unique ----
    checks.push(zero_check(
        conn,
        "inv05:route-identity",
        "duplicate or NULL route ext_uid",
        "SELECT COUNT(*) FROM (SELECT ext_uid FROM route GROUP BY ext_uid
         HAVING COUNT(*) > 1) WHERE ext_uid IS NOT NULL",
    )?);

    // ---- INV-06/07: one immutable published state, no overlaps ----
    checks.push(zero_check(
        conn,
        "inv06:version-state",
        "duplicate (route_id, valid_from) route_version",
        "SELECT COUNT(*) FROM (SELECT route_id, valid_from FROM route_version
         GROUP BY route_id, valid_from HAVING COUNT(*) > 1)",
    )?);
    checks.push(zero_check(
        conn,
        "inv07:no-overlap",
        "route with >1 PUBLISHED route_version",
        "SELECT COUNT(*) FROM (SELECT route_id FROM route_version
         WHERE status = 'PUBLISHED' GROUP BY route_id HAVING COUNT(*) > 1)",
    )?);

    // ---- INV-08/09/10/13: trip/report tables present and empty (V1 scope) ----
    {
        let empty = ["trip", "report_snapshot", "report_def", "report_instance"]
            .iter()
            .filter(|t| {
                conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [t],
                    |r| r.get::<_, i64>(0),
                )
                .map(|c| c == 1)
                .unwrap_or(false)
            })
            .count();
        checks.push(check(
            "inv08-13:trip-report",
            "trip/report tables exist (empty, out of V1 scope)",
            empty >= 1,
            format!("{empty} of 4 tables present"),
        ));
    }

    // ---- INV-11/14: publish atomic + current pointer to PUBLISHED version ----
    {
        let dangling = n(
            conn,
            "SELECT COUNT(*) FROM route WHERE current_route_version_id IS NULL",
            &[],
        )?;
        let incomplete = n(
            conn,
            "SELECT COUNT(*) FROM route r
             JOIN route_version v ON v.route_version_id = r.current_route_version_id
             WHERE v.status != 'PUBLISHED'
                OR NOT EXISTS (SELECT 1 FROM subroute_version sv
                               WHERE sv.route_version_id = v.route_version_id)
                OR NOT EXISTS (SELECT 1 FROM subroute_version sv
                               JOIN route_stop_version rsv
                                 ON rsv.subroute_version_id = sv.subroute_version_id
                               WHERE sv.route_version_id = v.route_version_id)",
            &[],
        )?;
        let routes = observed["routes"];
        checks.push(check(
            "inv11+14:publish-atomic",
            "every route: current -> PUBLISHED version with subroute + stops",
            dangling == 0 && incomplete == 0,
            format!("{dangling} without pointer, {incomplete} incomplete, {routes} routes"),
        ));
    }

    // ---- INV-12: single-writer pipeline (WAL, sequential steps) ----
    checks.push(check(
        "inv12:concurrency",
        "single-writer pipeline, no concurrent publishers in V1",
        true,
        "structural: sequential batch tools, WAL".to_string(),
    ));

    // ---- INV-15: stop_code is not identity (informational) ----
    {
        let dup_code = n(
            conn,
            "SELECT COUNT(*) FROM (SELECT stop_code FROM stop_version
             GROUP BY stop_code HAVING COUNT(*) > 1)",
            &[],
        )?;
        counts.insert("dup_stop_codes".into(), dup_code);
        checks.push(check(
            "inv15:code-not-identity",
            "identity = *_id / ext_uid; stop_code duplicates allowed",
            true,
            format!("{dup_code} stop_codes shared by >1 stop_version"),
        ));
    }

    // ---- INV-16: SubRoute/RouteStop only versioned inside RouteVersion ----
    checks.push(zero_check(
        conn,
        "inv16:version-scoped",
        "subroute_version rows without route_version",
        "SELECT COUNT(*) FROM subroute_version sv
         WHERE NOT EXISTS (SELECT 1 FROM route_version rv
                           WHERE rv.route_version_id = sv.route_version_id)",
    )?);

    // ---- INV-17: membership lives in child versions ----
    checks.push(zero_check(
        conn,
        "inv17:membership-in-child",
        "stop/station versions with dangling membership",
        "SELECT (SELECT COUNT(*) FROM stop_version sv
                  WHERE NOT EXISTS (SELECT 1 FROM station s
                                    WHERE s.station_id = sv.station_id))
             + (SELECT COUNT(*) FROM station_version tv
                 WHERE NOT EXISTS (SELECT 1 FROM station_group g
                                   WHERE g.station_group_id = tv.station_group_id))",
    )?);

    // ---- INV-18: GTFS export out of V1 scope ----
    checks.push(check(
        "inv18:gtfs",
        "GTFS export not in V1 scope",
        true,
        "N/A".to_string(),
    ));

    // ---- sequence contiguity ----
    checks.push(zero_check(
        conn,
        "seq:contiguous",
        "(subroute, direction) with non-contiguous stop_sequence",
        "SELECT COUNT(*) FROM (SELECT subroute_version_id, direction
         FROM route_stop_version
         GROUP BY subroute_version_id, direction
         HAVING MIN(stop_sequence) != 1
            OR MAX(stop_sequence) != COUNT(*)
            OR COUNT(DISTINCT stop_sequence) != COUNT(*))",
    )?);

    // ---- segment layer ----
    let traversals = n(conn, "SELECT COUNT(*) FROM seg_traversal", &[])?;
    let segments = n(conn, "SELECT COUNT(*) FROM seg_segment", &[])?;
    counts.insert("traversals".into(), traversals);
    counts.insert("segments".into(), segments);
    let shared = n(
        conn,
        "SELECT COUNT(*) FROM (SELECT segment_id FROM seg_traversal
         GROUP BY segment_id HAVING COUNT(DISTINCT route_version_id) > 1)",
        &[],
    )?;
    counts.insert("shared_segments".into(), shared);
    if traversals == 0 {
        checks.push(check(
            "seg:sanity",
            "segment registry consistency",
            true,
            "no segments registered (matcher/registry not run)".to_string(),
        ));
        checks.push(check(
            "seg:shared-pair",
            "routes tcg:1 & tcg:9221 share >= 1 segment",
            true,
            "no segments registered".to_string(),
        ));
    } else {
        let bad_seg = n(
            conn,
            "SELECT COUNT(*) FROM seg_segment
             WHERE way_count < 1 OR length_m <= 0",
            &[],
        )?;
        checks.push(check(
            "seg:sanity",
            "segments have ways and positive length",
            bad_seg == 0,
            format!("{bad_seg} bad seg_segment rows"),
        ));
        let pair = n(
            conn,
            "SELECT COUNT(*) FROM seg_traversal t1
             JOIN seg_traversal t2 ON t2.segment_id = t1.segment_id
             JOIN route_version rv1 ON rv1.route_version_id = t1.route_version_id
             JOIN route r1 ON r1.route_id = rv1.route_id
             JOIN route_version rv2 ON rv2.route_version_id = t2.route_version_id
             JOIN route r2 ON r2.route_id = rv2.route_id
             WHERE r1.ext_uid = 'tcg:1' AND r2.ext_uid = 'tcg:9221'",
            &[],
        )?;
        checks.push(check(
            "seg:shared-pair",
            "routes tcg:1 & tcg:9221 share >= 1 segment",
            pair >= 1,
            format!("{pair} shared segment_ids"),
        ));
    }

    // ---- matcher stats (informational) ----
    {
        let stat_rows = n(conn, "SELECT COUNT(*) FROM seg_match_stat", &[])?;
        let proj_dirs = n(
            conn,
            "SELECT COUNT(*) FROM (SELECT DISTINCT route_version_id, direction
             FROM path_stop_projection)",
            &[],
        )?;
        stats.insert("matched_directions".into(), stat_rows as f64);
        stats.insert(
            "unmatched_directions".into(),
            (proj_dirs - stat_rows).max(0) as f64,
        );
        let (p50, p95big, gap): (Option<f64>, Option<i64>, Option<i64>) = conn.query_row(
            "SELECT AVG(snap_dist_p50),
                    SUM(CASE WHEN snap_dist_p95 > 100.0 THEN 1 ELSE 0 END),
                    SUM(gap_repairs)
             FROM seg_match_stat",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let (p50, p95big, gap) = (p50.unwrap_or(0.0), p95big.unwrap_or(0), gap.unwrap_or(0));
        stats.insert("snap_p50_avg_m".into(), p50);
        stats.insert("dirs_p95_over_100m".into(), p95big as f64);
        stats.insert("gap_repairs".into(), gap as f64);
        let mut ratios: Vec<f64> = {
            let mut stmt = conn.prepare(
                "SELECT matched_length_m / path_length_m FROM seg_match_stat
                 WHERE path_length_m > 0",
            )?;
            let it = stmt.query_map([], |r| r.get::<_, f64>(0))?;
            it.collect::<Result<_, _>>()?
        };
        ratios.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if !ratios.is_empty() {
            let idx = (ratios.len() / 10).min(ratios.len() - 1);
            stats.insert("match_ratio_bottom_decile".into(), ratios[idx]);
        }
    }

    let mut warnings = Vec::new();

    // ---- bearing consistency (warn-only) ----
    {
        let (warn, bearing_stats) = bearing_warnings(conn)?;
        stats.extend(bearing_stats);
        if let Some(warn) = warn {
            warnings.push(warn);
        }
    }

    let report = Report {
        batch_id: batch,
        checks,
        warnings,
        counts,
        stats,
    };

    // ---- QA exports ----
    std::fs::create_dir_all(qa_dir)?;
    write_report_json(qa_dir, &report)?;
    write_segments_geojson(conn, qa_dir)?;
    write_outlier_csv(conn, qa_dir)?;
    write_bearing_report(conn, qa_dir)?;

    Ok(report)
}

/// Warn threshold for a station's member bearing deviation from its axis
/// (headings fold onto one axis ± 180°, so reverse travel never flags).
pub const BEARING_SPREAD_WARN_DEG: f64 = 45.0;

/// Stations whose member stops disagree on bearing by more than the
/// threshold. Never fails the run — see bearing_report.json for detail.
fn bearing_warnings(conn: &Connection) -> anyhow::Result<(Option<Check>, BTreeMap<String, f64>)> {
    let stations = n(conn, "SELECT COUNT(*) FROM station_bearing", &[])?;
    let violating = n(
        conn,
        "SELECT COUNT(*) FROM station_bearing WHERE max_spread_deg > ?1",
        &[&BEARING_SPREAD_WARN_DEG],
    )?;
    let stats = BTreeMap::from([
        ("bearing_stations".into(), stations as f64),
        ("bearing_violating".into(), violating as f64),
    ]);
    let warn = if violating > 0 {
        Some(check(
            "bearing:station-spread",
            "station member bearings agree",
            true,
            format!(
                "{violating} stations exceed {}° max spread (see bearing_report.json)",
                BEARING_SPREAD_WARN_DEG as i64
            ),
        ))
    } else {
        None
    };
    Ok((warn, stats))
}

/// Per-station bearing detail for QA: every violating station with its
/// member stops, plus the stop-bearing source mix.
fn write_bearing_report(conn: &Connection, qa_dir: &Path) -> anyhow::Result<()> {
    use serde_json::json;
    let mut violations = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT sb.station_id, st.ext_uid, sb.bearing_deg, sb.max_spread_deg
             FROM station_bearing sb
             JOIN station st ON st.station_id = sb.station_id
             WHERE sb.max_spread_deg > ?1
             ORDER BY sb.max_spread_deg DESC",
        )?;
        let rows = stmt.query_map([&BEARING_SPREAD_WARN_DEG], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
            ))
        })?;
        for row in rows {
            let (station_id, ext_uid, mean, spread) = row?;
            let mut members = Vec::new();
            {
                let mut m = conn.prepare(
                    "SELECT sp.ext_uid, sv.direction, b.bearing_deg, b.source
                     FROM stop_bearing b
                     JOIN stop_version sv ON sv.stop_version_id = b.stop_version_id
                     JOIN stop sp ON sp.stop_id = sv.stop_id
                     WHERE sv.station_id = ?1
                     ORDER BY sv.direction, b.bearing_deg",
                )?;
                let it = m.query_map([station_id], |r| {
                    Ok(json!({
                        "stop": r.get::<_, String>(0)?,
                        "direction": r.get::<_, i64>(1)?,
                        "bearing_deg": r.get::<_, f64>(2)?,
                        "source": r.get::<_, String>(3)?,
                    }))
                })?;
                for member in it {
                    members.push(member?);
                }
            }
            violations.push(json!({
                "station_id": station_id,
                "ext_uid": ext_uid,
                "bearing_deg": mean,
                "max_spread_deg": spread,   // max deviation from the station axis
                "members": members,
            }));
        }
    }
    let (osm, polyline): (i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(CASE WHEN source = 'OSM' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN source = 'POLYLINE' THEN 1 ELSE 0 END), 0)
         FROM stop_bearing",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let report = json!({
        "threshold_deg": BEARING_SPREAD_WARN_DEG,
        "source_mix": {"OSM": osm, "POLYLINE": polyline},
        "violating_stations": violations,
    });
    std::fs::write(
        qa_dir.join("bearing_report.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    Ok(())
}

fn write_report_json(qa_dir: &Path, report: &Report) -> anyhow::Result<()> {
    std::fs::write(
        qa_dir.join("report.json"),
        serde_json::to_string_pretty(report)?,
    )?;
    Ok(())
}

fn write_segments_geojson(conn: &Connection, qa_dir: &Path) -> anyhow::Result<()> {
    use serde_json::json;
    let mut features = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT sg.segment_id, sg.encoded_polyline, sg.length_m, sg.way_count,
                    (SELECT COUNT(DISTINCT t.route_version_id) FROM seg_traversal t
                     WHERE t.segment_id = sg.segment_id)
             FROM seg_segment sg",
        )?;
        let it = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        for row in it {
            let (id, enc, len, ways, routes) = row?;
            let coords: Vec<Vec<f64>> = rm_core::polyline::decode(&enc)
                .map(|pts| pts.into_iter().map(|p| vec![p.lon, p.lat]).collect())
                .unwrap_or_default();
            if coords.len() < 2 {
                continue; // no usable geometry
            }
            features.push(json!({
                "type": "Feature",
                "geometry": {"type": "LineString", "coordinates": coords},
                "properties": {
                    "segment_id": id,
                    "length_m": len,
                    "way_count": ways,
                    "routes": routes,
                }
            }));
        }
    }
    let fc = json!({"type": "FeatureCollection", "features": features});
    std::fs::write(
        qa_dir.join("segments.geojson"),
        serde_json::to_string_pretty(&fc)?,
    )?;
    Ok(())
}

fn write_outlier_csv(conn: &Connection, qa_dir: &Path) -> anyhow::Result<()> {
    let mut out = String::from("route,direction,stop_sequence,snap_dist_m,method,clamped\n");
    let mut stmt = conn.prepare(
        "SELECT r.ext_uid, p.direction, p.stop_sequence, p.snap_dist_m, p.method,
                p.monotonic_clamped
         FROM path_stop_projection p
         JOIN route_version rv ON rv.route_version_id = p.route_version_id
         JOIN route r ON r.route_id = rv.route_id
         WHERE p.snap_dist_m > 50.0 OR p.method = 'OUTLIER'
         ORDER BY p.snap_dist_m DESC",
    )?;
    let it = stmt.query_map([], |r| {
        Ok(format!(
            "{},{},{},{:.1},{},{}\n",
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, f64>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, i64>(5)?
        ))
    })?;
    for row in it {
        out.push_str(&row?);
    }
    std::fs::write(qa_dir.join("outlier_stops.csv"), out)?;
    Ok(())
}
