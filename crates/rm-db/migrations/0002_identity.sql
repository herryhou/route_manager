-- 0002_identity: ext_uid idempotent lookup for stop + station_group,
-- provenance flag on station_group_version.

ALTER TABLE stop ADD COLUMN ext_uid TEXT NOT NULL DEFAULT '';
CREATE UNIQUE INDEX idx_stop_ext_uid ON stop(ext_uid);

ALTER TABLE station_group ADD COLUMN ext_uid TEXT NOT NULL DEFAULT '';
CREATE UNIQUE INDEX idx_station_group_ext_uid ON station_group(ext_uid);

-- group provenance: 'derived:same-name<80m' | 'singleton'
ALTER TABLE station_group_version ADD COLUMN source TEXT NOT NULL DEFAULT 'derived:same-name<80m';
