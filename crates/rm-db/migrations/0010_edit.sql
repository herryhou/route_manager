-- 0010_edit: manual edit audit trail + staleness marks (docs/web-edit-mode.md).
-- source NULL = pipeline origin, 'manual' = user edit.

ALTER TABLE stop_version ADD COLUMN source TEXT;
ALTER TABLE station_version ADD COLUMN source TEXT;

CREATE TABLE edit_log (
    edit_id     INTEGER PRIMARY KEY,
    ts          TEXT NOT NULL,
    editor      TEXT NOT NULL,
    entity      TEXT NOT NULL,          -- 'stop' | 'station' | ...
    entity_id   INTEGER NOT NULL,       -- stop_id / station_id (stable id)
    action      TEXT NOT NULL,          -- 'update' | 'move_station' | 'undo' | 'create'
    changes     TEXT NOT NULL,          -- JSON {field: {old, new}}
    note        TEXT,
    undo_of     INTEGER REFERENCES edit_log(edit_id),
    version_before INTEGER,             -- version row closed by this edit
    version_after  INTEGER              -- version row made current (undo: re-validates version_before)
);

CREATE TABLE stale_mark (
    table_name    TEXT NOT NULL,
    key           TEXT NOT NULL,        -- unit the refresh step consumes ('*' = whole table)
    cause_edit_id INTEGER NOT NULL REFERENCES edit_log(edit_id),
    marked_at     TEXT NOT NULL,
    PRIMARY KEY (table_name, key)
);
