//! Split stations whose member stops carry opposing headings into
//! per-carriageway stations.
//!
//! Poles across a dual carriageway share a stop name and sit well inside
//! the 80 m clustering radius, so both sides land in one station —
//! coordinates cannot separate them. Member headings,
//! derived from matched OSM way runs, are the per-side signal:
//! headings are clustered on the circle and every gap wider than
//! [`SPLIT_GAP_DEG`] cuts a new station. The parent station keeps the
//! largest cluster (ties keep the smaller heading), so no station row is
//! ever left dangling; members without a bearing always stay on the
//! parent. Child stations reuse the parent's name/coords/group with a
//! `:heading` ext_uid suffix and its members' mean coordinates; the parent
//! likewise moves to its retained cluster's mean, which keeps re-runs
//! idempotent and the recomputed same-name grouping unchanged.

use std::collections::HashMap;

use anyhow::Result;
use rm_core::geo::circular_mean_deg;
use rusqlite::OptionalExtension;

/// Circular gap between member headings that separates two stations.
pub const SPLIT_GAP_DEG: f64 = 90.0;

#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    pub stations_split: usize,
    pub stations_created: usize,
    pub stops_moved: usize,
}

/// Split every current station whose beared members form 2+ heading
/// clusters. Idempotent: re-running with unchanged bearings moves nothing.
pub fn run(conn: &rusqlite::Connection) -> Result<Summary> {
    let mut summary = Summary::default();
    let now = rm_db::now_rfc3339();

    // current station versions (name/coords/group copied to children)
    struct StationRow {
        ext_uid: String,
        group_id: i64,
        name: String,
        lat: f64,
        lon: f64,
        valid_from: String,
    }
    let mut stations: HashMap<i64, StationRow> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT s.station_id, s.ext_uid, v.station_group_id,
                    v.station_name, v.latitude, v.longitude, v.valid_from
             FROM station s
             JOIN station_version v
               ON v.station_id = s.station_id AND v.valid_to IS NULL",
        )?;
        let it = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                StationRow {
                    ext_uid: r.get(1)?,
                    group_id: r.get(2)?,
                    name: r.get(3)?,
                    lat: r.get(4)?,
                    lon: r.get(5)?,
                    valid_from: r.get(6)?,
                },
            ))
        })?;
        for row in it {
            let (id, row) = row?;
            stations.insert(id, row);
        }
    }

    // current members per station, bearing optional; member coords for
    // placing child stations on their own carriageway
    let mut members: HashMap<i64, Vec<(i64, Option<f64>)>> = HashMap::new();
    let mut coords: HashMap<i64, (f64, f64)> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT s.station_id, s.stop_version_id, b.bearing_deg,
                    s.latitude, s.longitude
             FROM stop_version s
             LEFT JOIN stop_bearing b ON b.stop_version_id = s.stop_version_id
             WHERE s.valid_to IS NULL
             ORDER BY s.station_id",
        )?;
        let it = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<f64>>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
            ))
        })?;
        for row in it {
            let (sid, svid, bearing, lat, lon) = row?;
            members.entry(sid).or_default().push((svid, bearing));
            coords.insert(svid, (lat, lon));
        }
    }

    let mut order: Vec<i64> = stations.keys().copied().collect();
    order.sort_unstable();

    let tx = conn.unchecked_transaction()?;
    for station_id in order {
        let row = &stations[&station_id];
        let Some(ms) = members.get(&station_id) else { continue };
        let headed: Vec<(i64, f64)> = ms
            .iter()
            .filter_map(|(svid, b)| b.map(|b| (*svid, b)))
            .collect();
        let mut clusters = bearing_clusters(&headed);
        if clusters.len() < 2 {
            continue;
        }
        // parent keeps the largest cluster; ties keep the smaller heading,
        // children follow in ascending heading order
        clusters.sort_by(|a, b| {
            b.len().cmp(&a.len())
                .then_with(|| cluster_mean(a).partial_cmp(&cluster_mean(b)).unwrap())
        });

        let mut created_here = 0;
        let mut moved_here = 0;
        for cluster in &clusters[1..] {
            let uid = format!("{}:{}", row.ext_uid, cluster_mean(cluster).round() as i64);
            let (clat, clon) = cluster_coord(cluster, &coords, (row.lat, row.lon));
            let child_id = match tx
                .query_row(
                    "SELECT station_id FROM station WHERE ext_uid = ?1",
                    [&uid],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?
            {
                Some(id) => id,
                None => {
                    let id = rm_db::mint(&tx, "station")?;
                    tx.execute(
                        "INSERT INTO station (station_id, ext_uid, created_at)
                         VALUES (?1, ?2, ?3)",
                        rusqlite::params![id, uid, now],
                    )?;
                    let sv_id = rm_db::mint(&tx, "station_version")?;
                    tx.execute(
                        "INSERT INTO station_version
                            (station_version_id, station_id, station_group_id,
                             station_name, latitude, longitude, valid_from, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        rusqlite::params![
                            sv_id, id, row.group_id, row.name, clat, clon,
                            row.valid_from, now
                        ],
                    )?;
                    created_here += 1;
                    id
                }
            };
            for (svid, _) in cluster {
                // station_id guard: re-runs must not count already-moved stops
                moved_here += tx.execute(
                    "UPDATE stop_version SET station_id = ?1
                     WHERE stop_version_id = ?2 AND station_id = ?3",
                    rusqlite::params![child_id, svid, station_id],
                )?;
            }
        }
        if moved_here > 0 {
            // the parent sits on the carriageway it keeps: its old coords
            // may be the cluster's first occurrence, i.e. the other side
            let (plat, plon) = cluster_coord(&clusters[0], &coords, (row.lat, row.lon));
            tx.execute(
                "UPDATE station_version SET latitude = ?1, longitude = ?2
                 WHERE station_id = ?3 AND valid_to IS NULL",
                rusqlite::params![plat, plon, station_id],
            )?;
            summary.stations_split += 1;
            summary.stations_created += created_here;
            summary.stops_moved += moved_here;
        }
    }
    tx.commit()?;
    Ok(summary)
}

/// Circular mean of a cluster's headings (0.0 for an empty cluster, which
/// callers never split).
fn cluster_mean(cluster: &[(i64, f64)]) -> f64 {
    circular_mean_deg(&cluster.iter().map(|(_, b)| *b).collect::<Vec<_>>()).unwrap_or(0.0)
}

/// Arithmetic mean of a cluster's member coords — the child station sits on
/// its own carriageway, not the parent's. Falls back to the parent coords
/// when no member carries any (impossible today; guards future schemas).
fn cluster_coord(
    cluster: &[(i64, f64)],
    coords: &HashMap<i64, (f64, f64)>,
    fallback: (f64, f64),
) -> (f64, f64) {
    let mut sum = (0.0, 0.0);
    let mut n = 0usize;
    for (svid, _) in cluster {
        if let Some(&(lat, lon)) = coords.get(svid) {
            sum = (sum.0 + lat, sum.1 + lon);
            n += 1;
        }
    }
    if n == 0 {
        fallback
    } else {
        (sum.0 / n as f64, sum.1 / n as f64)
    }
}

/// Cluster headings on the circle, cutting at every gap wider than
/// [`SPLIT_GAP_DEG`]; a single cluster when no gap qualifies. Inputs need
/// not be sorted; ids ride along untouched.
fn bearing_clusters(headings: &[(i64, f64)]) -> Vec<Vec<(i64, f64)>> {
    let n = headings.len();
    if n < 2 {
        return vec![headings.to_vec()];
    }
    let mut hs = headings.to_vec();
    hs.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    let gap = |i: usize| (hs[(i + 1) % n].1 - hs[i].1 + 360.0) % 360.0;
    let widest = (0..n).fold(0usize, |best, i| {
        if gap(i) > gap(best) {
            i
        } else {
            best
        }
    });
    if gap(widest) <= SPLIT_GAP_DEG {
        return vec![hs];
    }
    // walk the circle starting just after the widest gap
    let start = (widest + 1) % n;
    let mut out: Vec<Vec<(i64, f64)>> = vec![Vec::new()];
    for k in 0..n {
        if k > 0 {
            let prev = (start + k - 1) % n;
            if gap(prev) > SPLIT_GAP_DEG {
                out.push(Vec::new());
            }
        }
        out.last_mut().unwrap().push(hs[(start + k) % n]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: i64, b: f64) -> (i64, f64) {
        (id, b)
    }

    fn clusters(v: &[(i64, f64)]) -> Vec<Vec<i64>> {
        bearing_clusters(v)
            .iter()
            .map(|c| c.iter().map(|(id, _)| *id).collect())
            .collect()
    }

    #[test]
    fn antipodal_pair_splits() {
        let v = vec![m(1, 150.4), m(2, 329.5), m(3, 150.4), m(4, 329.5)];
        let c = clusters(&v);
        assert_eq!(c, vec![vec![1, 3], vec![2, 4]]);
    }

    #[test]
    fn chained_corner_stays_one() {
        // gaps 80/80/200: only the 200 cut applies -> one cluster
        let v = vec![m(1, 0.0), m(2, 80.0), m(3, 160.0)];
        assert_eq!(clusters(&v), vec![vec![1, 2, 3]]);
    }

    #[test]
    fn three_way_intersection_splits_three() {
        // equal 120° gaps: cut at each, walk starts after the first widest
        let v = vec![m(1, 0.0), m(2, 120.0), m(3, 240.0)];
        assert_eq!(clusters(&v), vec![vec![2], vec![3], vec![1]]);
    }

    #[test]
    fn wraparound_cut_lands_in_one_cluster() {
        // 350 and 10 are 20° apart across north
        let v = vec![m(1, 350.0), m(2, 10.0), m(3, 170.0)];
        assert_eq!(clusters(&v), vec![vec![1, 2], vec![3]]);
    }

    #[test]
    fn gap_of_exactly_90_does_not_split() {
        let v = vec![m(1, 10.0), m(2, 100.0), m(3, 190.0)]; // gaps 90/90/180
        // widest gap 180 cuts; then 90/90 are not cuts -> single cluster
        assert_eq!(clusters(&v), vec![vec![1, 2, 3]]);
    }

    #[test]
    fn few_or_identical_headings_stay_one() {
        assert_eq!(clusters(&[m(1, 10.0)]), vec![vec![1]]);
        assert_eq!(clusters(&[]), vec![vec![]]);
        assert_eq!(
            clusters(&[m(1, 10.0), m(2, 10.0), m(3, 10.1)]),
            vec![vec![1, 2, 3]]
        );
    }
}
