-- 0011_drop_segment: segment registry removed.
-- Piece-level data (path_stop_projection, seg_geometry_draft, seg_match_way,
-- seg_match_stat) stays: station-pair way lists are derived by intersecting
-- the stop chainage window with seg_match_way runs. See
-- docs/segment-corridor-split.md for the removed design.

DROP TABLE seg_traversal;
DROP TABLE seg_segment_way;
DROP TABLE seg_segment;
