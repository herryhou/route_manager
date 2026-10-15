//! Stale-mark protocol: edits insert `stale_mark` rows naming derived
//! datasets whose contents no longer match their inputs; the refresh
//! pipelines snapshot the marks, rebuild, and clear exactly what they
//! processed. "Never green over stale data" runs through this module —
//! writers, consumers, and the UI's stale badges all go here.

use anyhow::Result;
use rusqlite::{params, Connection};

/// A derived dataset that edits can invalidate. The string is the
/// `stale_mark.table_name` spelling — this enum is its only owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleTable {
    StopBearing,
    StationBearing,
    PathStopProjection,
    SegMatch,
}

impl StaleTable {
    pub fn as_str(self) -> &'static str {
        match self {
            StaleTable::StopBearing => "stop_bearing",
            StaleTable::StationBearing => "station_bearing",
            StaleTable::PathStopProjection => "path_stop_projection",
            StaleTable::SegMatch => "seg_match",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        [
            StaleTable::StopBearing,
            StaleTable::StationBearing,
            StaleTable::PathStopProjection,
            StaleTable::SegMatch,
        ]
        .into_iter()
        .find(|t| t.as_str() == name)
    }
}

/// One mark as it existed at snapshot time. The `(rowid, cause_edit_id)`
/// pair is the clear token: a mark re-inserted onto a reused rowid carries
/// a different cause and survives the clear.
#[derive(Debug, Clone)]
pub struct StaleMark {
    pub rowid: i64,
    pub cause_edit_id: i64,
    pub table: StaleTable,
    pub key: String,
}

/// Insert or replace a mark. Re-marks replace on the `(table_name, key)`
/// primary key: fresh rowid, freshest `cause_edit_id` wins.
pub fn mark(
    tx: &Connection,
    table: StaleTable,
    key: &str,
    cause_edit_id: i64,
    marked_at: &str,
) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO stale_mark (table_name, key, cause_edit_id, marked_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![table.as_str(), key, cause_edit_id, marked_at],
    )?;
    Ok(())
}

/// Snapshot the current marks for `tables` in rowid order (deterministic —
/// a rerun walks the same sequence).
pub fn snapshot(conn: &Connection, tables: &[StaleTable]) -> Result<Vec<StaleMark>> {
    let list = tables
        .iter()
        .map(|t| format!("'{}'", t.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT rowid, table_name, key, cause_edit_id FROM stale_mark
         WHERE table_name IN ({list}) ORDER BY rowid"
    ))?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (rowid, table, key, cause_edit_id) = row?;
        Ok(StaleMark {
            rowid,
            table: StaleTable::from_name(&table).expect("snapshot filters known tables"),
            key,
            cause_edit_id,
        })
    })
    .collect()
}

/// Guarded clear of one snapshot mark: deletes only if the mark was not
/// re-written since the snapshot (fresh cause_edit_id survives). Returns
/// whether a row was cleared.
pub fn clear_if_unchanged(tx: &Connection, rowid: i64, cause_edit_id: i64) -> Result<bool> {
    let n = tx.execute(
        "DELETE FROM stale_mark WHERE rowid = ?1 AND cause_edit_id = ?2",
        params![rowid, cause_edit_id],
    )?;
    Ok(n > 0)
}

/// Clear a snapshot's `(rowid, cause_edit_id)` tokens inside the caller's
/// transaction; returns how many were still unchanged.
pub fn clear_all_if_unchanged(tx: &Connection, tokens: &[(i64, i64)]) -> Result<usize> {
    let mut cleared = 0;
    for &(rowid, cause) in tokens {
        cleared += usize::from(clear_if_unchanged(tx, rowid, cause)?);
    }
    Ok(cleared)
}

/// Marks waiting for a refresh run — the UI's stale counter.
pub fn pending_count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))?)
}

/// `seg_match` key: one direction of one route. Writers and the match
/// stale run share this spelling.
pub fn seg_match_key(route_id: i64, direction: i64) -> String {
    format!("{route_id}:{direction}")
}

/// Inverse of [`seg_match_key`]; `None` on malformed keys.
pub fn parse_seg_match_key(key: &str) -> Option<(i64, i64)> {
    let (r, d) = key.split_once(':')?;
    Some((r.parse().ok()?, d.parse().ok()?))
}

/// Wildcard key: the whole table's derived data is stale.
const WHOLE_TABLE: &str = "*";

/// Mark the whole `stop_bearing` table stale (position edits/undos mint new
/// versions whose bearings must be re-derived).
pub fn mark_bearings_stale(tx: &Connection, cause_edit_id: i64, now: &str) -> Result<()> {
    mark(tx, StaleTable::StopBearing, WHOLE_TABLE, cause_edit_id, now)
}

/// Mark the whole `station_bearing` table stale.
pub fn mark_station_bearings_stale(tx: &Connection, cause_edit_id: i64, now: &str) -> Result<()> {
    mark(
        tx,
        StaleTable::StationBearing,
        WHOLE_TABLE,
        cause_edit_id,
        now,
    )
}

/// Mark the derived data that depends on a stop's position stale: one
/// `path_stop_projection` mark per route serving `stop_version_id` (whose
/// `route_stop_version` rows must already point at it) plus the whole
/// `station_bearing` table (`stop_bearing` is marked unconditionally —
/// see `mark_bearings_stale`). Returns the marked route ids; the caller
/// reports them as the outcome's `stale_routes`.
pub fn mark_position_stale(
    tx: &Connection,
    stop_version_id: i64,
    cause_edit_id: i64,
    now: &str,
) -> Result<Vec<i64>> {
    let mut stale_routes = Vec::new();
    {
        let mut stmt = tx.prepare(
            "SELECT DISTINCT rv.route_id
             FROM route_stop_version rsv
             JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
             JOIN route_version rv ON rv.route_version_id = sv.route_version_id
             WHERE rsv.stop_version_id = ?1",
        )?;
        let rows = stmt.query_map([stop_version_id], |r| r.get::<_, i64>(0))?;
        for rid in rows {
            stale_routes.push(rid?);
        }
    }
    for rid in &stale_routes {
        mark(
            tx,
            StaleTable::PathStopProjection,
            &rid.to_string(),
            cause_edit_id,
            now,
        )?;
    }
    mark_station_bearings_stale(tx, cause_edit_id, now)?;
    Ok(stale_routes)
}

/// Mark every derived dataset that depends on the direction's polyline
/// stale. Keys use route_id (stable across route versions); the match
/// stale run resolves the current route_version at recompute time.
pub fn mark_line_stale(
    tx: &Connection,
    route_id: i64,
    direction: i64,
    cause_edit_id: i64,
    now: &str,
) -> Result<()> {
    mark(
        tx,
        StaleTable::SegMatch,
        &seg_match_key(route_id, direction),
        cause_edit_id,
        now,
    )?;
    mark(
        tx,
        StaleTable::PathStopProjection,
        &route_id.to_string(),
        cause_edit_id,
        now,
    )?;
    mark_bearings_stale(tx, cause_edit_id, now)?;
    mark_station_bearings_stale(tx, cause_edit_id, now)
}

/// Routes marked stale that serve this stop — the stop plate's projection
/// badge. Same serving-routes join as `mark_position_stale`, intersected
/// with `path_stop_projection` marks; ordered by route_id.
pub fn projection_routes_for_stop(conn: &Connection, stop_version_id: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT CAST(key AS INTEGER) FROM stale_mark
         WHERE table_name = 'path_stop_projection' AND key IN (
             SELECT DISTINCT CAST(rv.route_id AS TEXT)
             FROM route_stop_version rsv
             JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
             JOIN route_version rv ON rv.route_version_id = sv.route_version_id
             WHERE rsv.stop_version_id = ?1)
         ORDER BY 1",
    )?;
    let it = stmt.query_map([stop_version_id], |r| r.get::<_, i64>(0))?;
    Ok(it.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// True when any bearing mark is pending — the plate's bearing badge.
pub fn bearings_stale(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM stale_mark
                       WHERE table_name IN ('stop_bearing','station_bearing'))",
        [],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;

    /// Minimal edit_log row — stale_mark.cause_edit_id has an FK to it.
    fn seed_edit(conn: &Connection, edit_id: i64) {
        conn.execute(
            "INSERT INTO edit_log (edit_id, ts, editor, entity, entity_id, action, changes)
             VALUES (?1, '2026-10-15T00:00:00Z', 'herry', 'stop', 1, 'move', '{}')",
            [edit_id],
        )
        .unwrap();
    }

    /// Minimal route chain: route 1 (current version 20) → subroute 1 →
    /// stop 2 via route_stop_version. Enough for mark_position_stale.
    fn seed_serving_route(conn: &Connection) {
        let t = "2026-10-15T00:00:00Z";
        conn.execute_batch(&format!(
            "INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', '{t}');
             INSERT INTO stop (stop_id, created_at) VALUES (1, '{t}');
             INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
               VALUES (2, 1, 1, 'S1', 'X', 0, 24.0, 120.0, '{t}', '{t}');
             INSERT INTO route (route_id, route_code, ext_uid, created_at, updated_at)
               VALUES (1, '3', 'tcg:3', '{t}', '{t}');
             INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at)
               VALUES (20, 1, 1, '{t}', 'PUBLISHED', '{t}');
             UPDATE route SET current_route_version_id = 20 WHERE route_id = 1;
             INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at) VALUES (1, 1, 'A', '{t}');
             INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
               VALUES (1, 20, 1, '往北', '{t}');
             INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
               VALUES (1, 1, 2, 0, 1, '{t}');"
        ))
        .unwrap();
    }

    #[test]
    fn mark_replaces_on_same_table_and_key() {
        let conn = open_in_memory().unwrap();
        seed_edit(&conn, 1);
        seed_edit(&conn, 2);
        mark(&conn, StaleTable::StopBearing, "*", 1, "t1").unwrap();
        mark(&conn, StaleTable::StopBearing, "*", 2, "t2").unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "(table_name, key) is the primary key");
        let (cause, at): (i64, String) = conn
            .query_row("SELECT cause_edit_id, marked_at FROM stale_mark", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((cause, at.as_str()), (2, "t2"), "freshest wins");
    }

    #[test]
    fn snapshot_filters_tables_and_walks_rowid_order() {
        let conn = open_in_memory().unwrap();
        for id in [1, 2, 3] {
            seed_edit(&conn, id);
        }
        mark(&conn, StaleTable::StopBearing, "*", 1, "t").unwrap();
        mark(&conn, StaleTable::SegMatch, "7:1", 2, "t").unwrap();
        mark(&conn, StaleTable::PathStopProjection, "7", 3, "t").unwrap();
        let marks = snapshot(&conn, &[StaleTable::StopBearing, StaleTable::SegMatch]).unwrap();
        assert_eq!(marks.len(), 2);
        assert_eq!(marks[0].table, StaleTable::StopBearing);
        assert_eq!(marks[0].key, "*");
        assert_eq!(marks[0].cause_edit_id, 1);
        assert_eq!(marks[1].table, StaleTable::SegMatch);
        assert_eq!(marks[1].key, "7:1");
    }

    #[test]
    fn clear_if_unchanged_survives_remark() {
        let conn = open_in_memory().unwrap();
        seed_edit(&conn, 1);
        seed_edit(&conn, 9);
        mark(&conn, StaleTable::StopBearing, "*", 1, "t1").unwrap();
        let m = &snapshot(&conn, &[StaleTable::StopBearing]).unwrap()[0];
        // re-mark the same key mid-rebuild: fresh rowid + cause
        mark(&conn, StaleTable::StopBearing, "*", 9, "t2").unwrap();
        let cleared = clear_if_unchanged(&conn, m.rowid, m.cause_edit_id).unwrap();
        assert!(
            !cleared,
            "old (rowid, cause) token must not delete the re-mark"
        );
        assert_eq!(pending_count(&conn).unwrap(), 1);
        // the fresh token clears
        let m2 = &snapshot(&conn, &[StaleTable::StopBearing]).unwrap()[0];
        assert!(clear_if_unchanged(&conn, m2.rowid, m2.cause_edit_id).unwrap());
        assert_eq!(pending_count(&conn).unwrap(), 0);
    }

    #[test]
    fn clear_all_if_unchanged_counts_cleared() {
        let conn = open_in_memory().unwrap();
        seed_edit(&conn, 1);
        seed_edit(&conn, 2);
        mark(&conn, StaleTable::StopBearing, "*", 1, "t").unwrap();
        mark(&conn, StaleTable::StationBearing, "*", 2, "t").unwrap();
        let marks = snapshot(
            &conn,
            &[StaleTable::StopBearing, StaleTable::StationBearing],
        )
        .unwrap();
        let tokens: Vec<(i64, i64)> = marks.iter().map(|m| (m.rowid, m.cause_edit_id)).collect();
        let tx = conn.unchecked_transaction().unwrap();
        assert_eq!(clear_all_if_unchanged(&tx, &tokens).unwrap(), 2);
        tx.commit().unwrap();
        assert_eq!(pending_count(&conn).unwrap(), 0);
    }

    #[test]
    fn seg_match_key_roundtrips_and_rejects_garbage() {
        assert_eq!(seg_match_key(7, 1), "7:1");
        assert_eq!(parse_seg_match_key("7:1"), Some((7, 1)));
        assert_eq!(parse_seg_match_key("junk"), None);
        assert_eq!(parse_seg_match_key("7:x"), None);
        assert_eq!(parse_seg_match_key("x:1"), None);
    }

    #[test]
    fn mark_bearings_helpers_write_whole_table_keys() {
        let conn = open_in_memory().unwrap();
        seed_edit(&conn, 1);
        mark_bearings_stale(&conn, 1, "t").unwrap();
        mark_station_bearings_stale(&conn, 1, "t").unwrap();
        let rows: Vec<(String, String)> = conn
            .prepare("SELECT table_name, key FROM stale_mark ORDER BY table_name")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("station_bearing".to_string(), "*".to_string()),
                ("stop_bearing".to_string(), "*".to_string()),
            ]
        );
    }

    #[test]
    fn mark_position_stale_marks_serving_routes_plus_station_bearing() {
        let conn = open_in_memory().unwrap();
        seed_serving_route(&conn);
        seed_edit(&conn, 5);
        let routes = mark_position_stale(&conn, 2, 5, "t").unwrap();
        assert_eq!(routes, vec![1]);
        let rows: Vec<(String, String, i64)> = conn
            .prepare("SELECT table_name, key, cause_edit_id FROM stale_mark ORDER BY table_name")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("path_stop_projection".to_string(), "1".to_string(), 5),
                ("station_bearing".to_string(), "*".to_string(), 5),
            ]
        );
    }

    #[test]
    fn mark_position_stale_unknown_stop_still_marks_station_bearing() {
        let conn = open_in_memory().unwrap();
        seed_edit(&conn, 5);
        let routes = mark_position_stale(&conn, 999, 5, "t").unwrap();
        assert!(routes.is_empty());
        assert_eq!(pending_count(&conn).unwrap(), 1);
    }

    #[test]
    fn mark_line_stale_marks_all_four_datasets() {
        let conn = open_in_memory().unwrap();
        seed_edit(&conn, 5);
        mark_line_stale(&conn, 7, 1, 5, "t").unwrap();
        let rows: Vec<(String, String)> = conn
            .prepare("SELECT table_name, key FROM stale_mark ORDER BY table_name, key")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("path_stop_projection".to_string(), "7".to_string()),
                ("seg_match".to_string(), "7:1".to_string()),
                ("station_bearing".to_string(), "*".to_string()),
                ("stop_bearing".to_string(), "*".to_string()),
            ]
        );
    }

    #[test]
    fn projection_routes_for_stop_reads_marks_through_serving_join() {
        let conn = open_in_memory().unwrap();
        seed_serving_route(&conn);
        seed_edit(&conn, 5);
        // route 2 is marked but does not serve the stop; route 1 serves + marked
        mark(&conn, StaleTable::PathStopProjection, "1", 5, "t").unwrap();
        mark(&conn, StaleTable::PathStopProjection, "2", 5, "t").unwrap();
        assert_eq!(projection_routes_for_stop(&conn, 2).unwrap(), vec![1]);
    }

    #[test]
    fn bearings_stale_reflects_bearing_marks_only() {
        let conn = open_in_memory().unwrap();
        seed_edit(&conn, 5);
        seed_edit(&conn, 6);
        assert!(!bearings_stale(&conn).unwrap());
        // projection marks alone don't trip the bearing badge
        mark(&conn, StaleTable::PathStopProjection, "1", 5, "t").unwrap();
        assert!(!bearings_stale(&conn).unwrap());
        mark(&conn, StaleTable::StationBearing, "*", 6, "t").unwrap();
        assert!(bearings_stale(&conn).unwrap());
    }
}
