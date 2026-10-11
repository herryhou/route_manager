-- Whole-line polyline override (phase 2). One current override per
-- (xno, direction); unversioned — the row IS the current state and
-- `updated_at` doubles as the optimistic-concurrency token. No surrogate
-- PK: (xno, direction) is the identity, so no id_sequence involvement.
-- No `updated_by`: edit_log is the audit trail (house rule).
CREATE TABLE polyline_override (
  xno         TEXT NOT NULL,
  direction   INTEGER NOT NULL,
  encoded     TEXT NOT NULL,   -- full replacement geometry, ≥2 points
  updated_at  TEXT NOT NULL    -- = producing edit's ts (concurrency token)
);
-- exactly one current override per direction
CREATE UNIQUE INDEX polyline_override_dir ON polyline_override (xno, direction);
