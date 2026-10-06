-- 0006_match_dev: path→polyline deviation percentiles per direction —
-- the missing side of match quality (snap_dist_* cover polyline→path).

ALTER TABLE seg_match_stat ADD COLUMN dev_p50_m REAL;
ALTER TABLE seg_match_stat ADD COLUMN dev_p95_m REAL;
ALTER TABLE seg_match_stat ADD COLUMN dev_max_m REAL;
