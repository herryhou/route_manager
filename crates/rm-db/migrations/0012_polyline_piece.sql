-- 0012_polyline_piece: manual src-polyline pieces (phase 1: storage only).
-- One row per piece = current (unversioned); updated_at is the
-- optimistic-concurrency token and equals the producing edit's ts. History
-- is edit_log's `changes` (old/new encoded), not a version chain. See
-- docs/superpowers/specs/2026-10-10-polyline-piece-edit-design.md.
CREATE TABLE polyline_piece (
    piece_id         INTEGER PRIMARY KEY,   -- minted via id_sequence
    xno              TEXT NOT NULL,
    direction        INTEGER NOT NULL,
    encoded          TEXT NOT NULL,         -- Google 1e-5, ≥2 points
    anchor_start_lat REAL NOT NULL,
    anchor_start_lon REAL NOT NULL,
    anchor_end_lat   REAL NOT NULL,
    anchor_end_lon   REAL NOT NULL,
    updated_at       TEXT NOT NULL
);
