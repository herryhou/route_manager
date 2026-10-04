-- 0006_rename_route_stop: rename stop_of_route_version -> route_stop_version
-- (domain language RouteStop; TDX StopOfRoute term stays at the import boundary).

ALTER TABLE stop_of_route_version RENAME TO route_stop_version;

ALTER TABLE route_stop_version RENAME COLUMN stop_of_route_version_id TO route_stop_version_id;
ALTER TABLE path_stop_projection RENAME COLUMN stop_of_route_version_id TO route_stop_version_id;
ALTER TABLE seg_geometry_draft RENAME COLUMN from_stop_of_route_version_id TO from_route_stop_version_id;
ALTER TABLE seg_geometry_draft RENAME COLUMN to_stop_of_route_version_id TO to_route_stop_version_id;
ALTER TABLE seg_traversal RENAME COLUMN from_stop_of_route_version_id TO from_route_stop_version_id;
ALTER TABLE seg_traversal RENAME COLUMN to_stop_of_route_version_id TO to_route_stop_version_id;

-- SQLite has no ALTER INDEX RENAME
DROP INDEX idx_sorv_seq;
DROP INDEX idx_sorv_stop;
CREATE INDEX idx_rsv_seq ON route_stop_version(subroute_version_id, direction, stop_sequence);
CREATE INDEX idx_rsv_stop ON route_stop_version(subroute_version_id, stop_version_id);

UPDATE id_sequence SET table_name = 'route_stop_version' WHERE table_name = 'stop_of_route_version';
