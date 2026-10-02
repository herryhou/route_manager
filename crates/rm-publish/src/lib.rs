//! rm-publish: per-route atomic V1 publish (route_version + subroute_version
//! + stop_of_route_version + current pointer + outbox_event, one tx each).

use anyhow::Result;
use rusqlite::{Connection, Transaction};

#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    pub batch_id: i64,
    pub published: usize,
    pub skipped: usize,
    pub failed: usize,
    pub failures: Vec<(String, String)>, // (xno, reason)
    pub stop_of_route_rows: usize,
}

/// Publish every route in `batch` (numeric xno order). One tx per route:
/// a bad route rolls back alone and is reported; others proceed.
pub fn run(conn: &mut Connection, batch_id: i64) -> Result<Summary> {
    use rm_identity::xno_key;

    let valid_from: String = conn.query_row(
        "SELECT valid_from FROM import_batch WHERE batch_id = ?1",
        [batch_id],
        |r| r.get(0),
    )?;
    let now = rm_db::now_rfc3339();
    let mut summary = Summary {
        batch_id,
        ..Default::default()
    };

    let mut headers: Vec<(String, String, String)> = Vec::new(); // (xno, code, description)
    {
        let mut stmt = conn.prepare(
            "SELECT xno, route_code, description FROM src_route_header WHERE batch_id = ?1",
        )?;
        let rows = stmt.query_map([batch_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            headers.push(row?);
        }
    }
    headers.sort_by_key(|h| xno_key(&h.0));

    for (xno, _code, description) in &headers {
        match publish_route(conn, batch_id, xno, description, &valid_from, &now) {
            Ok(Some(rows)) => {
                summary.published += 1;
                summary.stop_of_route_rows += rows;
            }
            Ok(None) => summary.skipped += 1,
            Err(reason) => {
                summary.failed += 1;
                summary.failures.push((xno.clone(), reason.to_string()));
            }
        }
    }
    Ok(summary)
}

/// Ok(Some(rows)) published; Ok(None) skipped (already has current pointer).
fn publish_route(
    conn: &mut Connection,
    batch_id: i64,
    xno: &str,
    description: &str,
    valid_from: &str,
    now: &str,
) -> Result<Option<usize>> {
    let route_id: i64 = conn
        .query_row(
            "SELECT route_id FROM route WHERE ext_uid = ?1",
            [format!("tcg:{xno}")],
            |r| r.get(0),
        )
        .map_err(|_| anyhow::anyhow!("no route identity for xno {xno}"))?;

    let already: Option<i64> = conn
        .query_row(
            "SELECT current_route_version_id FROM route WHERE route_id = ?1",
            [route_id],
            |r| r.get(0),
        )
        .unwrap_or(None);
    if already.is_some() {
        return Ok(None);
    }

    // staged stops for this route, per direction in source order
    let mut staged: Vec<(i64, i64, i64)> = Vec::new(); // (direction, source_order_no, sid)
    {
        let mut stmt = conn.prepare(
            "SELECT direction, source_order_no, sid FROM src_stop_edge
             WHERE batch_id = ?1 AND xno = ?2
             ORDER BY direction, source_order_no, sid",
        )?;
        let rows = stmt.query_map(rusqlite::params![batch_id, xno], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            staged.push(row?);
        }
    }
    if staged.is_empty() {
        anyhow::bail!("no stops in batch {batch_id} for xno {xno}");
    }

    // resolve stop_version ids (identity must have minted them all)
    let mut resolved: Vec<(i64, i64, i64, i64)> = Vec::new(); // (direction, source_order_no, sid, stop_version_id)
    for (direction, order_no, sid) in &staged {
        let uid = rm_identity::stop_ext_uid(xno, *direction, *sid);
        let sv_id: i64 = conn
            .query_row(
                "SELECT sv.stop_version_id FROM stop s
                 JOIN stop_version sv ON sv.stop_id = s.stop_id
                 WHERE s.ext_uid = ?1",
                [uid.clone()],
                |r| r.get(0),
            )
            .map_err(|_| anyhow::anyhow!("no stop_version for {uid}"))?;
        resolved.push((*direction, *order_no, *sid, sv_id));
    }

    let tx = Transaction::new(&mut *conn, rusqlite::TransactionBehavior::Immediate)?;
    if let Err(e) = publish_tx(&tx, route_id, xno, description, valid_from, now, &resolved) {
        tx.rollback()?;
        return Err(e);
    }
    tx.commit()?;
    Ok(Some(resolved.len()))
}

fn publish_tx(
    tx: &rusqlite::Transaction,
    route_id: i64,
    xno: &str,
    description: &str,
    valid_from: &str,
    now: &str,
    resolved: &[(i64, i64, i64, i64)],
) -> Result<()> {
    use rusqlite::OptionalExtension;

    // version_no = max + 1; interval must not start before any existing one
    let (max_no, last_from): (i64, Option<String>) = tx
        .query_row(
            "SELECT COALESCE(MAX(version_no), 0),
                    (SELECT valid_from FROM route_version WHERE route_id = ?1
                     ORDER BY version_no DESC LIMIT 1)
             FROM route_version WHERE route_id = ?1",
            [route_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .unwrap_or((0, None));
    if let Some(lf) = &last_from {
        if valid_from < lf.as_str() {
            anyhow::bail!("valid_from {valid_from} before existing {lf} (overlap, spec §21)");
        }
    }

    let rv_id = rm_db::mint(tx, "route_version")?;
    tx.execute(
        "INSERT INTO route_version
            (route_version_id, route_id, version_no, valid_from, valid_to, status, created_at, published_at)
         VALUES (?1, ?2, ?3, ?4, NULL, 'PUBLISHED', ?5, ?5)",
        rusqlite::params![rv_id, route_id, max_no + 1, valid_from, now],
    )?;

    let subroute_id: i64 = tx.query_row(
        "SELECT subroute_id FROM subroute WHERE route_id = ?1",
        [route_id],
        |r| r.get(0),
    )?;
    let sv_id = rm_db::mint(tx, "subroute_version")?;
    tx.execute(
        "INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![sv_id, rv_id, subroute_id, description, now],
    )?;

    let mut seq_of_dir: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    for (direction, order_no, _sid, stop_version_id) in resolved {
        let seq = seq_of_dir.entry(*direction).or_insert(0);
        *seq += 1;
        tx.execute(
            "INSERT INTO stop_of_route_version
                (stop_of_route_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, source_order_no, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                rm_db::mint(tx, "stop_of_route_version")?,
                sv_id,
                stop_version_id,
                direction,
                *seq,
                order_no,
                now
            ],
        )?;
    }

    tx.execute(
        "UPDATE route SET current_route_version_id = ?1, revision = revision + 1, updated_at = ?2
         WHERE route_id = ?3",
        rusqlite::params![rv_id, now, route_id],
    )?;

    let stops = resolved.len();
    let payload = serde_json::json!({
        "route_id": route_id,
        "route_version_id": rv_id,
        "version_no": max_no + 1,
        "xno": xno,
        "stops": stops,
        "published_at": now,
    });
    tx.execute(
        "INSERT INTO outbox_event (event_id, aggregate_type, aggregate_id, event_type, payload, created_at)
         VALUES (?1, 'route', ?2, 'route.published', ?3, ?4)",
        rusqlite::params![rm_db::mint(tx, "outbox_event")?, route_id, payload.to_string(), now],
    )?;
    Ok(())
}
