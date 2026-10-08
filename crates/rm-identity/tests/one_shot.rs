//! One-shot import: identity refuses non-OPEN batches and closes on success.

use rm_db::open_in_memory;
use rm_ingest::testfix;

#[test]
fn identity_consumes_open_batch_then_refuses_reuse() {
    let dir = tempfile::tempdir().unwrap();
    testfix::make_fixture(dir.path());
    let conn = open_in_memory().unwrap();
    let sum = rm_ingest::run(&conn, dir.path()).unwrap();
    let batch = sum.batch_id;

    let first = rm_identity::run(&conn, batch).unwrap();
    assert!(first.stops > 0);

    let status: String = conn
        .query_row("SELECT status FROM import_batch WHERE batch_id = ?1", [batch], |r| r.get(0))
        .unwrap();
    assert_eq!(status, "CLOSED");

    let second = rm_identity::run(&conn, batch);
    assert!(second.is_err(), "closed batch must be refused");
}
