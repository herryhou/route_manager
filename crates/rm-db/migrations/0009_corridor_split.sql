-- 0009_corridor_split: one segment per corridor within a station pair.
-- Cuts of a pair are clustered by length; each cluster is its own segment
-- (segment_id includes the corridor key) and stores the pair grouping key
-- for QA. Registry fully rebuilds seg_* each run, no backfill needed.

ALTER TABLE seg_segment ADD COLUMN pair_key TEXT NOT NULL DEFAULT '';
ALTER TABLE seg_segment ADD COLUMN corridor_key TEXT NOT NULL DEFAULT '';
