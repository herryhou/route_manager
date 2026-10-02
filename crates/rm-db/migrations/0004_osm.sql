-- 0004_osm: road graph loaded from a local OSM PBF extract.

CREATE TABLE osm_meta (
    key   TEXT PRIMARY KEY,   -- pbf_sha256, pbf_file, imported_at, highway_whitelist
    value TEXT NOT NULL
);

CREATE TABLE osm_way (
    osm_way_id INTEGER PRIMARY KEY,
    highway    TEXT NOT NULL,
    oneway     INTEGER NOT NULL DEFAULT 0,   -- 1 yes, -1 reverse, 0 no
    name       TEXT NOT NULL DEFAULT ''
);

-- node references in way order (travel order matters for directed segments)
CREATE TABLE osm_way_node (
    osm_way_id INTEGER NOT NULL,
    ordinal    INTEGER NOT NULL,
    osm_node_id INTEGER NOT NULL,
    PRIMARY KEY (osm_way_id, ordinal),
    FOREIGN KEY (osm_way_id) REFERENCES osm_way(osm_way_id)
);
CREATE INDEX idx_own_node ON osm_way_node(osm_node_id);

CREATE TABLE osm_node (
    osm_node_id INTEGER PRIMARY KEY,
    lat REAL NOT NULL,
    lon REAL NOT NULL
);
