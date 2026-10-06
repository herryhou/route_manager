-- 0007_current_views: read-only convenience views over the versioned model.
-- "Current" = the open version (valid_to IS NULL; the pipeline guarantees at
-- most one open version per identity) or, for routes, the current_route_version_id
-- pointer. Ad-hoc queries and future read paths get one flat table per concept
-- instead of repeating the version joins. Writers keep using the base tables.

-- current stop rows (one per open stop version)
CREATE VIEW stop_current AS
SELECT stop_version_id, stop_id, station_id, stop_code, stop_name,
       direction, latitude, longitude, valid_from, created_at
FROM stop_version
WHERE valid_to IS NULL;

-- current station rows
CREATE VIEW station_current AS
SELECT station_version_id, station_id, station_group_id, station_name,
       latitude, longitude, valid_from, created_at
FROM station_version
WHERE valid_to IS NULL;

-- current station-group rows
CREATE VIEW station_group_current AS
SELECT station_group_version_id, station_group_id, station_group_name,
       source, valid_from, created_at
FROM station_group_version
WHERE valid_to IS NULL;

-- routes joined to their current route_version
CREATE VIEW route_current AS
SELECT r.route_id, r.route_code, r.ext_uid, r.revision,
       rv.route_version_id, rv.version_no, rv.valid_from, rv.status,
       rv.published_at
FROM route r
JOIN route_version rv ON rv.route_version_id = r.current_route_version_id;

-- the route→route_version→subroute_version→route_stop_version chain,
-- flattened for the current route version: ordered stops per direction
CREATE VIEW route_stop_current AS
SELECT r.route_id, r.route_code,
       rv.route_version_id,
       sv.subroute_version_id, sv.subroute_name,
       rsv.route_stop_version_id, rsv.stop_version_id,
       rsv.direction, rsv.stop_sequence, rsv.source_order_no
FROM route r
JOIN route_version rv ON rv.route_version_id = r.current_route_version_id
JOIN subroute_version sv ON sv.route_version_id = rv.route_version_id
JOIN route_stop_version rsv ON rsv.subroute_version_id = sv.subroute_version_id;
