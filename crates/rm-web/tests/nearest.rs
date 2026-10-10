//! Nearest-station lookup: map-click target pick for the regroup flow.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rm_web::router_with;
use rm_web::state::AppState;
use tower::ServiceExt; // oneshot

use rusqlite::Connection;

fn state(db: std::path::PathBuf) -> AppState {
    AppState::new(db)
}

/// Two stations ≈203 m apart on the 24.0°N parallel (group chain only —
/// the nearest query touches station/station_version and nothing else).
fn fixture(conn: &Connection) {
    conn.execute(
        "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO station_group_version (station_group_version_id, station_group_id, station_group_name, source, valid_from, created_at)
         VALUES (1, 1, 'g', 'capture', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    for (id, uid, name, lon) in [
        (1, "tcg:node:1", "A", 120.0),
        (2, "tcg:node:2", "B", 120.002),
    ] {
        conn.execute(
            "INSERT INTO station (station_id, ext_uid, created_at) VALUES (?1, ?2, '2026-01-01T00:00:00Z')",
            rusqlite::params![id, uid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
             VALUES (?1, ?2, 1, ?3, 24.0, ?4, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![id, id, name, lon],
        )
        .unwrap();
    }
}

fn nearest_state() -> (tempfile::TempDir, axum::Router) {
    let dir = tempfile::tempdir().unwrap();
    let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
    fixture(&conn);
    drop(conn);
    let app = router_with(state(dir.path().join("t.db")));
    (dir, app)
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let v: serde_json::Value =
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, v)
}

#[tokio::test]
async fn nearest_returns_closest_station_with_distance() {
    let (_dir, app) = nearest_state();
    // midway point: A ≈ 50.9 m west, B ≈ 152 m east
    let (s, v) = get(&app, "/api/stations/nearest?lat=24.0&lon=120.0005&max_m=100").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["station_id"], 1);
    assert_eq!(v["station_name"], "A");
    assert_eq!(v["latitude"], 24.0);
    assert_eq!(v["longitude"], 120.0);
    let d = v["distance_m"].as_f64().unwrap();
    assert!((45.0..=56.0).contains(&d), "distance {d}");
}

#[tokio::test]
async fn nearest_misses_when_nothing_within_max_m() {
    let (_dir, app) = nearest_state();
    let (s, v) = get(&app, "/api/stations/nearest?lat=24.0&lon=120.0005&max_m=40").await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("no station within 40 m"),
        "{v}"
    );
}

/// The bbox prefilter must be a provable superset of the radius circle: a
/// station just inside max_m due north (worst case — the bbox lat side is
/// the tight one) is found, not 404'd. 0.000899° north ≈ 99.96 m, inside
/// max_m=100 but outside a lat box cut at 100/111320 (0.00089831°).
#[tokio::test]
async fn nearest_bbox_covers_radius_band_due_north() {
    let dir = tempfile::tempdir().unwrap();
    let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
    conn.execute(
        "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO station_group_version (station_group_version_id, station_group_id, station_group_name, source, valid_from, created_at)
         VALUES (1, 1, 'g', 'capture', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO station (station_id, ext_uid, created_at) VALUES (3, 'tcg:node:3', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
         VALUES (3, 3, 1, 'N', 24.000899, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    drop(conn);
    let app = router_with(state(dir.path().join("t.db")));
    let (s, v) = get(&app, "/api/stations/nearest?lat=24.0&lon=120.0&max_m=100").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["station_id"], 3);
    let d = v["distance_m"].as_f64().unwrap();
    assert!((99.0..=100.0).contains(&d), "distance {d}");
}

#[tokio::test]
async fn nearest_validates_params() {
    let (_dir, app) = nearest_state();
    for q in [
        "lon=120.0&max_m=100",            // missing lat
        "lat=24.0&max_m=100",             // missing lon
        "lat=91&lon=120.0&max_m=100",     // lat out of range
        "lat=24.0&lon=181&max_m=100",     // lon out of range
        "lat=24.0&lon=120.0",             // missing max_m
        "lat=24.0&lon=120.0&max_m=501",   // over cap
        "lat=24.0&lon=120.0&max_m=-1",    // negative
        "lat=x&lon=120.0&max_m=100",      // unparseable
    ] {
        let (s, v) = get(&app, &format!("/api/stations/nearest?{q}")).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "query {q}: {v}");
    }
}
