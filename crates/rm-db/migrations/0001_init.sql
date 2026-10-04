-- 0001_init: versioned model per docs/DB_Schema_Spec.md (concretized for SQLite:
-- BIGINT->INTEGER, TIMESTAMP->TEXT RFC3339 UTC, DECIMAL lat/lon->REAL)
-- plus pipeline-internal staging/allocator tables per the approved plan.

PRAGMA foreign_keys = ON;

-- ===== identity tables (spec §3, §3.1, §6) =====

CREATE TABLE route (
    route_id                  INTEGER PRIMARY KEY,
    route_code                TEXT NOT NULL,
    ext_uid                   TEXT NOT NULL UNIQUE,   -- pipeline: tcg:{xno}
    current_route_version_id  INTEGER NULL,
    revision                  INTEGER NOT NULL DEFAULT 0,
    created_at                TEXT NOT NULL,
    updated_at                TEXT NOT NULL,
    FOREIGN KEY (current_route_version_id)
        REFERENCES route_version(route_version_id)
);

CREATE TABLE subroute (
    subroute_id    INTEGER PRIMARY KEY,
    route_id       INTEGER NOT NULL,
    subroute_code  TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    FOREIGN KEY (route_id) REFERENCES route(route_id),
    UNIQUE (route_id, subroute_code)
);

CREATE TABLE stop (
    stop_id     INTEGER PRIMARY KEY,
    created_at  TEXT NOT NULL
);

CREATE TABLE station (
    station_id  INTEGER PRIMARY KEY,
    ext_uid     TEXT NOT NULL UNIQUE,   -- pipeline: tcg:node:{node.id}
    created_at  TEXT NOT NULL
);

CREATE TABLE station_group (
    station_group_id  INTEGER PRIMARY KEY,
    created_at        TEXT NOT NULL
);

-- ===== version tables (spec §4, §7, §9) =====

CREATE TABLE route_version (
    route_version_id  INTEGER PRIMARY KEY,
    route_id          INTEGER NOT NULL,
    version_no        INTEGER NOT NULL,
    valid_from        TEXT NOT NULL,
    valid_to          TEXT NULL,
    status            TEXT NOT NULL,   -- DRAFT|PUBLISHED|SUPERSEDED|CANCELLED
    created_at        TEXT NOT NULL,
    published_at      TEXT NULL,
    created_by        INTEGER NULL,
    FOREIGN KEY (route_id) REFERENCES route(route_id),
    UNIQUE (route_id, version_no)
);

CREATE TABLE subroute_version (
    subroute_version_id  INTEGER PRIMARY KEY,
    route_version_id     INTEGER NOT NULL,
    subroute_id          INTEGER NOT NULL,
    subroute_name        TEXT NOT NULL,
    created_at           TEXT NOT NULL,
    FOREIGN KEY (route_version_id) REFERENCES route_version(route_version_id),
    FOREIGN KEY (subroute_id) REFERENCES subroute(subroute_id),
    UNIQUE (route_version_id, subroute_id)
);

-- Cyclic routes may visit a stop twice (spec §9): deliberately NO
-- UNIQUE(subroute_version_id, direction, stop_version_id).
CREATE TABLE route_stop_version (
    route_stop_version_id  INTEGER PRIMARY KEY,
    subroute_version_id       INTEGER NOT NULL,
    stop_version_id           INTEGER NOT NULL,
    direction                 INTEGER NOT NULL,   -- 0=去程 1=返程
    stop_sequence             INTEGER NOT NULL,
    source_order_no           INTEGER NULL,       -- pipeline: raw orderNo pre-renumber
    created_at                TEXT NOT NULL,
    FOREIGN KEY (subroute_version_id)
        REFERENCES subroute_version(subroute_version_id),
    FOREIGN KEY (stop_version_id) REFERENCES stop_version(stop_version_id),
    UNIQUE (subroute_version_id, direction, stop_sequence)
);

CREATE TABLE stop_version (
    stop_version_id  INTEGER PRIMARY KEY,
    stop_id          INTEGER NOT NULL,
    station_id       INTEGER NOT NULL,
    stop_code        TEXT NOT NULL,
    stop_name        TEXT NOT NULL,
    direction        INTEGER NOT NULL,
    latitude         REAL NOT NULL,
    longitude        REAL NOT NULL,
    valid_from       TEXT NOT NULL,
    valid_to         TEXT NULL,
    created_at       TEXT NOT NULL,
    FOREIGN KEY (stop_id) REFERENCES stop(stop_id),
    FOREIGN KEY (station_id) REFERENCES station(station_id)
);

CREATE TABLE station_version (
    station_version_id  INTEGER PRIMARY KEY,
    station_id          INTEGER NOT NULL,
    station_group_id    INTEGER NOT NULL,
    station_name        TEXT NOT NULL,
    latitude            REAL NOT NULL,
    longitude           REAL NOT NULL,
    valid_from          TEXT NOT NULL,
    valid_to            TEXT NULL,
    created_at          TEXT NOT NULL,
    FOREIGN KEY (station_id) REFERENCES station(station_id),
    FOREIGN KEY (station_group_id) REFERENCES station_group(station_group_id)
);

CREATE TABLE station_group_version (
    station_group_version_id  INTEGER PRIMARY KEY,
    station_group_id          INTEGER NOT NULL,
    station_group_name        TEXT NOT NULL,
    valid_from                TEXT NOT NULL,
    valid_to                  TEXT NULL,
    created_at                TEXT NOT NULL,
    FOREIGN KEY (station_group_id)
        REFERENCES station_group(station_group_id)
);

-- ===== trip / report / outbox (spec §11-14; empty, schema completeness) =====

CREATE TABLE trip (
    trip_id            INTEGER PRIMARY KEY,
    route_id           INTEGER NOT NULL,
    route_version_id   INTEGER NOT NULL,
    subroute_id        INTEGER NOT NULL,
    direction          INTEGER NOT NULL,
    occurred_at        TEXT NOT NULL,
    created_at         TEXT NOT NULL,
    FOREIGN KEY (route_id) REFERENCES route(route_id),
    FOREIGN KEY (route_version_id) REFERENCES route_version(route_version_id),
    FOREIGN KEY (subroute_id) REFERENCES subroute(subroute_id)
);

CREATE TABLE report_snapshot (
    report_id       INTEGER PRIMARY KEY,
    snapshot_at     TEXT NOT NULL,
    snapshot_type   TEXT NOT NULL,
    status          TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    completed_at    TEXT NULL
);

CREATE TABLE report_route_snapshot (
    report_id          INTEGER NOT NULL,
    route_id           INTEGER NOT NULL,
    route_version_id   INTEGER NOT NULL,
    PRIMARY KEY (report_id, route_id),
    FOREIGN KEY (report_id) REFERENCES report_snapshot(report_id),
    FOREIGN KEY (route_id) REFERENCES route(route_id),
    FOREIGN KEY (route_version_id) REFERENCES route_version(route_version_id)
);

CREATE TABLE report_station_group_snapshot (
    report_id                INTEGER NOT NULL,
    station_group_id         INTEGER NOT NULL,
    station_group_version_id INTEGER NOT NULL,
    PRIMARY KEY (report_id, station_group_id),
    FOREIGN KEY (report_id) REFERENCES report_snapshot(report_id),
    FOREIGN KEY (station_group_id) REFERENCES station_group(station_group_id),
    FOREIGN KEY (station_group_version_id)
        REFERENCES station_group_version(station_group_version_id)
);

CREATE TABLE outbox_event (
    event_id         INTEGER PRIMARY KEY,
    aggregate_type   TEXT NOT NULL,
    aggregate_id     INTEGER NOT NULL,
    event_type       TEXT NOT NULL,
    payload          TEXT NOT NULL,
    created_at       TEXT NOT NULL,
    published_at     TEXT NULL
);

-- ===== indexes (spec §15) =====

CREATE INDEX idx_route_version_route_valid ON route_version(route_id, valid_from);
CREATE INDEX idx_subroute_version_rv ON subroute_version(route_version_id);
CREATE INDEX idx_rsv_seq ON route_stop_version(subroute_version_id, direction, stop_sequence);
CREATE INDEX idx_rsv_stop ON route_stop_version(subroute_version_id, stop_version_id);
CREATE INDEX idx_stop_version_valid ON stop_version(stop_id, valid_from);
CREATE INDEX idx_station_version_valid ON station_version(station_id, valid_from);
CREATE INDEX idx_sgv_valid ON station_group_version(station_group_id, valid_from);
CREATE INDEX idx_trip_rv ON trip(route_version_id);
CREATE INDEX idx_trip_occurred ON trip(occurred_at);
CREATE INDEX idx_rrs_report ON report_route_snapshot(report_id);
CREATE INDEX idx_rsgs_report ON report_station_group_snapshot(report_id);
CREATE INDEX idx_outbox_pub ON outbox_event(published_at, created_at);

-- ===== pipeline: id allocator =====
-- mint() = UPDATE id_sequence SET next_value = next_value + N RETURNING,
-- auto-seeded per table on first use. Ids are never reused (INV/I02).
CREATE TABLE id_sequence (
    table_name  TEXT PRIMARY KEY,
    next_value  INTEGER NOT NULL
);

-- ===== pipeline: import batch =====

CREATE TABLE import_batch (
    batch_id    INTEGER PRIMARY KEY,
    source_dir  TEXT NOT NULL,
    file_count  INTEGER NOT NULL,
    captured_at TEXT NOT NULL,     -- when the raw data was captured
    valid_from  TEXT NOT NULL,     -- validity start stamped onto versions
    status      TEXT NOT NULL,     -- OPEN|CLOSED|FAILED
    created_at  TEXT NOT NULL
);

-- per-file audit (sha256 of every input file, one tx per plan)
CREATE TABLE src_file (
    batch_id  INTEGER NOT NULL,
    kind      TEXT NOT NULL,       -- route_header|route_detail|provider|...
    name      TEXT NOT NULL,
    sha256    TEXT NOT NULL,
    row_count INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (batch_id, kind, name)
);

-- ===== pipeline: staging (batch-scoped, no FKs by design) =====

CREATE TABLE src_route_header (
    batch_id     INTEGER NOT NULL,
    xno          TEXT NOT NULL,    -- route number, string form of Routes.id
    route_code   TEXT NOT NULL,    -- rider-facing name, e.g. "3"
    departure    TEXT NOT NULL,
    destination  TEXT NOT NULL,
    description  TEXT NOT NULL,
    seq          INTEGER NOT NULL,
    is_cycled    INTEGER NOT NULL,
    op_type      INTEGER NOT NULL,
    PRIMARY KEY (batch_id, xno)
);

CREATE TABLE src_stop_edge (
    batch_id        INTEGER NOT NULL,
    xno             TEXT NOT NULL,
    direction       INTEGER NOT NULL,   -- goBack 1->0, 2->1
    source_order_no INTEGER NOT NULL,   -- raw orderNo (gaps exist)
    ivr_no          INTEGER NULL,
    sid             INTEGER NOT NULL,
    node_id         TEXT NOT NULL,
    station_name    TEXT NOT NULL,
    lat             REAL NOT NULL,
    lon             REAL NOT NULL,
    lat_e6          INTEGER NOT NULL,
    lon_e6          INTEGER NOT NULL,
    PRIMARY KEY (batch_id, xno, direction, source_order_no, sid)
);

CREATE TABLE src_route_point (
    batch_id  INTEGER NOT NULL,
    xno       TEXT NOT NULL,
    direction INTEGER NOT NULL,
    encoded   TEXT NOT NULL,       -- Google-encoded polyline, '' when missing
    PRIMARY KEY (batch_id, xno, direction)
);

CREATE TABLE src_provider (
    batch_id      INTEGER NOT NULL,
    provider_code TEXT NOT NULL,
    name          TEXT NOT NULL,
    telephone     TEXT NOT NULL,
    op_type       INTEGER NOT NULL,
    PRIMARY KEY (batch_id, provider_code)
);

CREATE TABLE src_route_provider (
    batch_id      INTEGER NOT NULL,
    xno           TEXT NOT NULL,
    provider_code TEXT NOT NULL,
    PRIMARY KEY (batch_id, xno, provider_code)
);

CREATE TABLE src_main_side_route (
    batch_id  INTEGER NOT NULL,
    main_xno  TEXT NOT NULL,
    side_xno  TEXT NOT NULL,
    PRIMARY KEY (batch_id, main_xno, side_xno)
);

CREATE TABLE src_joint_operation (
    batch_id      INTEGER NOT NULL,
    bus_provider  TEXT NOT NULL,
    provider_code TEXT NOT NULL,
    route_xno     TEXT NOT NULL,
    PRIMARY KEY (batch_id, route_xno, provider_code)
);

-- ===== pipeline: derived link tables (filled by rm-identity) =====

CREATE TABLE route_link (
    main_route_id  INTEGER NOT NULL,
    side_route_id  INTEGER NOT NULL,
    PRIMARY KEY (main_route_id, side_route_id),
    FOREIGN KEY (main_route_id) REFERENCES route(route_id),
    FOREIGN KEY (side_route_id) REFERENCES route(route_id)
);

CREATE TABLE route_provider (
    route_id      INTEGER NOT NULL,
    provider_code TEXT NOT NULL,
    provider_name TEXT NOT NULL,
    PRIMARY KEY (route_id, provider_code),
    FOREIGN KEY (route_id) REFERENCES route(route_id)
);
