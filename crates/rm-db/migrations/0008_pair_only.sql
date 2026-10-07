-- 0008_pair_only: segment identity is the directed station pair; the
-- matched way list is an attribute, not part of identity. Per-route path
-- variants (dual carriageways, detours) are kept as data.

ALTER TABLE seg_segment ADD COLUMN variant_count INTEGER NOT NULL DEFAULT 1;
ALTER TABLE seg_segment ADD COLUMN length_spread_m REAL NOT NULL DEFAULT 0.0;
ALTER TABLE seg_traversal ADD COLUMN way_sig TEXT NOT NULL DEFAULT '';
