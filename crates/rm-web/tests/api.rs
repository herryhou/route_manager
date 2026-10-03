//! Integration tests for the rm-web API, driven through the router.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rm_web::router_with;
use rm_web::state::AppState;
use tower::ServiceExt;

fn app() -> axum::Router {
    router_with(AppState::new(std::path::PathBuf::from("/nonexistent.db")))
}

/// Fixture pipeline (ingest → identity → publish) on a temp-file DB;
/// returns the dir (keep alive) and the DB path the server can open read-only.
fn fixture_db() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    rm_ingest::testfix::make_fixture(&dir.path().join("src"));
    let db = dir.path().join("fixture.db");
    let mut conn = rm_db::open(&db).unwrap();
    let s = rm_ingest::run(&conn, &dir.path().join("src")).unwrap();
    rm_identity::run(&conn, s.batch_id).unwrap();
    rm_publish::run(&mut conn, s.batch_id).unwrap();
    (dir, db)
}

async fn get_json(app: &axum::Router, uri: &str) -> serde_json::Value {
    let res = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "GET {uri}");
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn root_serves_html() {
    let res = app()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/html"));
}

#[tokio::test]
async fn stats_reports_fixture_counts() {
    let (_dir, db) = fixture_db();
    let app = router_with(AppState::new(db));

    let v = get_json(&app, "/api/stats").await;
    let counts = &v["counts"];
    assert_eq!(counts["routes"].as_i64().unwrap(), 2, "fixture has 2 routes");
    for key in ["stops", "stations", "groups", "segments", "traversals"] {
        assert!(counts[key].is_i64(), "counts.{key} missing: {v}");
    }
    assert_eq!(counts["segments"].as_i64().unwrap(), 0, "no registry stage run");
    let m = &v["match"];
    assert_eq!(m["directions_with_stats"].as_i64().unwrap(), 0);
    assert!(m["directions_total"].as_i64().unwrap() >= 2);
    assert!(v["outlier_stops"].is_i64());
    assert_eq!(v["batch"]["batch_id"].as_i64().unwrap(), 1);
    assert!(v["batch"]["captured_at"].is_string());
}

#[tokio::test]
async fn unknown_api_path_is_404_json() {
    let res = app()
        .oneshot(
            Request::builder()
                .uri("/api/definitely-not-a-thing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
