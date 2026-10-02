//! rm-ingest: parse raw Taichung captures into src_* staging (one batch).

mod parse;
#[cfg(feature = "testfix")]
pub mod testfix;

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

pub use parse::Parsed;

/// What one ingest run wrote + warnings it noticed.
#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    pub batch_id: i64,
    pub routes: usize,
    pub stop_edges: usize,
    pub route_points: usize,
    pub providers: usize,
    pub route_providers: usize,
    pub main_side_links: usize,
    pub joint_operations: usize,
    pub warnings: Vec<String>,
}

/// Ingest `source` dir (routes.json, providers.json, main-side-routes.json,
/// jointOperationsBuses.json, raw/route_*.json) into staging under one tx.
///
/// Reuses an OPEN batch whose source_dir matches; otherwise mints one.
/// Idempotent per batch: existing staging rows for the batch are replaced.
/// Parse error: nothing written; any pre-existing reused batch -> FAILED.
pub fn run(conn: &Connection, source: &Path) -> Result<Summary> {
    let source_dir = source
        .to_str()
        .with_context(|| format!("non-utf8 source path {}", source.display()))?
        .to_string();

    // Reuse an OPEN batch for this source_dir, if one exists.
    let reusable: Option<i64> = conn
        .query_row(
            "SELECT batch_id FROM import_batch
             WHERE status = 'OPEN' AND source_dir = ?1
             ORDER BY batch_id DESC LIMIT 1",
            [&source_dir],
            |r| r.get(0),
        )
        .optional()?;

    let parsed = match parse::parse_dir(source) {
        Ok(p) => p,
        Err(e) => {
            if let Some(id) = reusable {
                rm_db::set_batch_status(conn, id, "FAILED")?;
            }
            return Err(e);
        }
    };

    let tx = conn.unchecked_transaction()?;
    let batch_id = match reusable {
        Some(id) => id,
        None => rm_db::create_batch(
            &tx,
            &source_dir,
            parsed.files.len() as i64,
            &rm_db::now_rfc3339(),
            &rm_db::now_rfc3339(),
        )?,
    };

    // Idempotent per batch: wipe previous staging for this batch first.
    for table in [
        "src_route_header",
        "src_stop_edge",
        "src_route_point",
        "src_provider",
        "src_route_provider",
        "src_main_side_route",
        "src_joint_operation",
        "src_file",
    ] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE batch_id = ?1"),
            [batch_id],
        )?;
    }

    for f in &parsed.files {
        tx.execute(
            "INSERT INTO src_file (batch_id, kind, name, sha256, row_count) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![batch_id, f.kind, f.name, f.sha256, f.row_count as i64],
        )?;
    }
    for h in &parsed.headers {
        tx.execute(
            "INSERT INTO src_route_header (batch_id, xno, route_code, departure, destination, description, seq, is_cycled, op_type)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                batch_id, h.xno, h.route_code, h.departure, h.destination, h.description,
                h.seq, h.is_cycled as i64, h.op_type
            ],
        )?;
        for pc in &h.provider_codes {
            tx.execute(
                "INSERT OR IGNORE INTO src_route_provider (batch_id, xno, provider_code) VALUES (?1, ?2, ?3)",
                params![batch_id, h.xno, pc],
            )?;
        }
    }
    for r in &parsed.raws {
        for s in &r.stops {
            tx.execute(
                "INSERT INTO src_stop_edge (batch_id, xno, direction, source_order_no, ivr_no, sid, node_id, station_name, lat, lon, lat_e6, lon_e6)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    batch_id,
                    r.xno,
                    s.direction,
                    s.order_no,
                    s.ivr_no,
                    s.sid,
                    s.node_id,
                    s.name,
                    s.lat,
                    s.lon,
                    (s.lat * 1e6).round() as i64,
                    (s.lon * 1e6).round() as i64,
                ],
            )?;
        }
        for (dir, enc) in [(0i64, &r.poly_go), (1i64, &r.poly_back)] {
            tx.execute(
                "INSERT INTO src_route_point (batch_id, xno, direction, encoded) VALUES (?1, ?2, ?3, ?4)",
                params![batch_id, r.xno, dir, enc],
            )?;
        }
    }
    for pv in &parsed.providers {
        tx.execute(
            "INSERT INTO src_provider (batch_id, provider_code, name, telephone, op_type) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![batch_id, pv.code, pv.name, pv.telephone, pv.op_type],
        )?;
    }
    for (main, side) in &parsed.main_side {
        tx.execute(
            "INSERT OR IGNORE INTO src_main_side_route (batch_id, main_xno, side_xno) VALUES (?1, ?2, ?3)",
            params![batch_id, main, side],
        )?;
    }
    for j in &parsed.joint {
        tx.execute(
            "INSERT OR IGNORE INTO src_joint_operation (batch_id, bus_provider, provider_code, route_xno) VALUES (?1, ?2, ?3, ?4)",
            params![batch_id, j.bus_provider, j.provider_code, j.route_xno],
        )?;
    }
    tx.commit()?;

    Ok(Summary {
        batch_id,
        routes: parsed.headers.len(),
        stop_edges: parsed.raws.iter().map(|r| r.stops.len()).sum(),
        route_points: parsed.raws.len() * 2,
        providers: parsed.providers.len(),
        route_providers: parsed.headers.iter().map(|h| h.provider_codes.len()).sum(),
        main_side_links: parsed.main_side.len(),
        joint_operations: parsed.joint.len(),
        warnings: parsed.warnings,
    })
}
