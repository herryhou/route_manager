-- 0005_bearing: derived departure headings for stop arrows and station
-- bearing consistency validation. Derived data, rebuilt per run — never
-- stored inside the immutable *_version tables.

-- heading of the bus leaving (arriving at, for the final) this stop,
-- along its route direction
CREATE TABLE stop_bearing (
    stop_version_id INTEGER PRIMARY KEY,
    bearing_deg     REAL NOT NULL,   -- [0, 360)
    source          TEXT NOT NULL,   -- OSM | POLYLINE
    computed_at     TEXT NOT NULL,
    FOREIGN KEY (stop_version_id) REFERENCES stop_version(stop_version_id)
);

-- aggregate over member stop bearings (stops whose station_id is this station)
CREATE TABLE station_bearing (
    station_id     INTEGER PRIMARY KEY,
    bearing_deg    REAL NOT NULL,    -- circular mean of member stops
    member_count   INTEGER NOT NULL, -- member stops with a bearing
    max_spread_deg REAL NOT NULL,    -- max pairwise angular difference
    computed_at    TEXT NOT NULL,
    FOREIGN KEY (station_id) REFERENCES station(station_id)
);
