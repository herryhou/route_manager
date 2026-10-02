-- 0003_segment: path projection, geometry drafts, matcher output,
-- segment registry (per approved plan).

-- stop -> path projection (chainage along the direction's decoded polyline)
CREATE TABLE path_stop_projection (
    stop_of_route_version_id  INTEGER PRIMARY KEY,
    route_version_id          INTEGER NOT NULL,
    direction                 INTEGER NOT NULL,
    stop_sequence             INTEGER NOT NULL,
    chainage_m                REAL NOT NULL,
    snap_dist_m               REAL NOT NULL,
    method                    TEXT NOT NULL,   -- PROJECTED|ENDPOINT|OUTLIER|NO_PATH
    monotonic_clamped         INTEGER NOT NULL DEFAULT 0,
    projected_lat             REAL NOT NULL,
    projected_lon             REAL NOT NULL,
    FOREIGN KEY (stop_of_route_version_id)
        REFERENCES stop_of_route_version(stop_of_route_version_id),
    FOREIGN KEY (route_version_id)
        REFERENCES route_version(route_version_id)
);
CREATE INDEX idx_psp_route ON path_stop_projection(route_version_id, direction, stop_sequence);

-- geometry between consecutive stops, cut from the path (or straight line)
CREATE TABLE seg_geometry_draft (
    subroute_version_id            INTEGER NOT NULL,
    direction                      INTEGER NOT NULL,
    ordinal                        INTEGER NOT NULL,  -- 1..N-1
    from_stop_of_route_version_id  INTEGER NOT NULL,
    to_stop_of_route_version_id    INTEGER NOT NULL,
    encoded_polyline               TEXT NOT NULL,
    length_m                       REAL NOT NULL,
    geometry_method                TEXT NOT NULL,     -- path|no_path
    PRIMARY KEY (subroute_version_id, direction, ordinal),
    FOREIGN KEY (from_stop_of_route_version_id)
        REFERENCES stop_of_route_version(stop_of_route_version_id),
    FOREIGN KEY (to_stop_of_route_version_id)
        REFERENCES stop_of_route_version(stop_of_route_version_id)
);

-- rm-osm-match: way runs along a direction path
CREATE TABLE seg_match_way (
    subroute_version_id  INTEGER NOT NULL,
    direction            INTEGER NOT NULL,
    ordinal              INTEGER NOT NULL,   -- run index along the path
    osm_way_id           INTEGER NOT NULL,
    entry_node_id        INTEGER NOT NULL,
    exit_node_id         INTEGER NOT NULL,
    cum_start_m          REAL NOT NULL,
    cum_end_m            REAL NOT NULL,
    PRIMARY KEY (subroute_version_id, direction, ordinal)
);

-- rm-osm-match: per route+direction matching quality
CREATE TABLE seg_match_stat (
    route_version_id  INTEGER NOT NULL,
    direction         INTEGER NOT NULL,
    stops             INTEGER NOT NULL,
    path_length_m     REAL NOT NULL,
    matched_length_m  REAL NOT NULL,
    snap_dist_p50     REAL,
    snap_dist_p95     REAL,
    snap_dist_max     REAL,
    outliers          INTEGER NOT NULL,
    gap_repairs       INTEGER NOT NULL,
    PRIMARY KEY (route_version_id, direction)
);

-- rm-registry: directed segment identity
CREATE TABLE seg_segment (
    segment_id         TEXT PRIMARY KEY,   -- blake3_128 hex
    start_station_id   INTEGER NOT NULL,
    end_station_id     INTEGER NOT NULL,
    encoded_polyline   TEXT NOT NULL,
    length_m           REAL NOT NULL,
    way_count          INTEGER NOT NULL,
    FOREIGN KEY (start_station_id) REFERENCES station(station_id),
    FOREIGN KEY (end_station_id) REFERENCES station(station_id)
);

CREATE TABLE seg_segment_way (
    segment_id   TEXT NOT NULL,
    ordinal      INTEGER NOT NULL,
    osm_way_id   INTEGER NOT NULL,
    length_m     REAL NOT NULL,
    PRIMARY KEY (segment_id, ordinal),
    FOREIGN KEY (segment_id) REFERENCES seg_segment(segment_id)
);

-- which route traversals use which segment
CREATE TABLE seg_traversal (
    route_version_id              INTEGER NOT NULL,
    subroute_version_id           INTEGER NOT NULL,
    direction                     INTEGER NOT NULL,
    from_stop_of_route_version_id INTEGER NOT NULL,
    to_stop_of_route_version_id   INTEGER NOT NULL,
    segment_id                    TEXT NOT NULL,
    PRIMARY KEY (from_stop_of_route_version_id, to_stop_of_route_version_id),
    FOREIGN KEY (segment_id) REFERENCES seg_segment(segment_id),
    FOREIGN KEY (route_version_id) REFERENCES route_version(route_version_id),
    FOREIGN KEY (subroute_version_id) REFERENCES subroute_version(subroute_version_id)
);

CREATE INDEX idx_trav_segment ON seg_traversal(segment_id);
