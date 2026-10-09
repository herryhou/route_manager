use anyhow::{Context, Result};
use rusqlite::Connection;

const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_init", include_str!("../migrations/0001_init.sql")),
    (
        "0002_identity",
        include_str!("../migrations/0002_identity.sql"),
    ),
    (
        "0003_segment",
        include_str!("../migrations/0003_segment.sql"),
    ),
    ("0004_osm", include_str!("../migrations/0004_osm.sql")),
    (
        "0005_bearing",
        include_str!("../migrations/0005_bearing.sql"),
    ),
    (
        "0006_match_dev",
        include_str!("../migrations/0006_match_dev.sql"),
    ),
    (
        "0007_current_views",
        include_str!("../migrations/0007_current_views.sql"),
    ),
    (
        "0008_pair_only",
        include_str!("../migrations/0008_pair_only.sql"),
    ),
    (
        "0009_corridor_split",
        include_str!("../migrations/0009_corridor_split.sql"),
    ),
    ("0010_edit", include_str!("../migrations/0010_edit.sql")),
    (
        "0011_drop_segment",
        include_str!("../migrations/0011_drop_segment.sql"),
    ),
];

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
    use crate::open_in_memory;

    #[test]
    fn migrations_are_ordered_and_named() {
        let names: Vec<&str> = super::MIGRATIONS.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            [
                "0001_init",
                "0002_identity",
                "0003_segment",
                "0004_osm",
                "0005_bearing",
                "0006_match_dev",
                "0007_current_views",
                "0008_pair_only",
                "0009_corridor_split",
                "0010_edit",
                "0011_drop_segment"
            ]
        );
    }

    #[test]
    fn edit_tables_exist_after_migrate() {
        let conn = open_in_memory().unwrap();
        for tbl in ["edit_log", "stale_mark"] {
            let n: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM pragma_table_info('{tbl}')"),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(n > 0, "{tbl} exists");
        }
        // version tables gain a nullable source column (NULL = pipeline)
        for tbl in ["stop_version", "station_version"] {
            let n: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM pragma_table_info('{tbl}') WHERE name = 'source'"
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "{tbl}.source exists");
        }
    }
}
