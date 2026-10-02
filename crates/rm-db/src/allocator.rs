use anyhow::Result;
use rusqlite::Connection;

/// Mint the next id for `table`'s id space. Auto-seeds at 1 on first use.
/// Ids are never reused (DB invariant I02) — allocation only moves forward.
///
/// Must be called inside the caller's transaction: the UPDATE...RETURNING
/// holds the row lock so concurrent minters serialize.
pub fn mint(conn: &Connection, table: &str) -> Result<i64> {
    match next(conn, table) {
        Ok(id) => Ok(id),
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            // Race-tolerant seed: another writer may have inserted first;
            // INSERT OR IGNORE then re-run the same UPDATE.
            conn.execute(
                "INSERT OR IGNORE INTO id_sequence (table_name, next_value) VALUES (?1, 1)",
                [table],
            )?;
            next(conn, table).map_err(Into::into)
        }
        Err(other) => Err(other.into()),
    }
}

fn next(conn: &Connection, table: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "UPDATE id_sequence SET next_value = next_value + 1
         WHERE table_name = ?1
         RETURNING next_value - 1",
        [table],
        |r| r.get(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;

    #[test]
    fn mint_is_monotonic() {
        let conn = open_in_memory().unwrap();
        let a = mint(&conn, "t").unwrap();
        let b = mint(&conn, "t").unwrap();
        assert_eq!((a, b), (1, 2));
    }
}
