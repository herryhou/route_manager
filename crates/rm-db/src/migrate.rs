use anyhow::{Context, Result};
use rusqlite::Connection;

const MIGRATIONS: &[(&str, &str)] = &[("0001_init", include_str!("../migrations/0001_init.sql"))];

/// Apply pending migrations in order. Safe to call repeatedly.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            name       TEXT NOT NULL,
            applied_at TEXT NOT NULL
        );",
    )
    .context("create schema_migrations")?;

    for (i, (name, sql)) in MIGRATIONS.iter().enumerate() {
        let version = (i + 1) as i64;
        let applied: bool = conn
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE version = ?1",
                [version],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if applied {
            continue;
        }
        conn.execute_batch(sql)
            .with_context(|| format!("apply migration {name}"))?;
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![version, name, crate::now_rfc3339()],
        )
        .context("record migration")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn migration_list_starts_at_0001() {
        assert_eq!(super::MIGRATIONS[0].0, "0001_init");
    }
}
