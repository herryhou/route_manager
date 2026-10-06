//! Debug one direction's match against the loaded graph.
use rm_osm_match::graph::RoadGraph;
use rm_osm_match::models::{BasicTransition, NewsonKrummEmission};
use rm_osm_match::viterbi;

fn main() -> anyhow::Result<()> {
    let xno = std::env::args().nth(1).expect("xno");
    let dir: i64 = std::env::args().nth(2).expect("dir").parse()?;
    let conn = rm_db::open(std::path::Path::new("data/taichung.db"))?;
    let g = RoadGraph::from_conn(&conn)?;
    let enc: String = conn.query_row(
        "SELECT encoded FROM src_route_point WHERE batch_id=1 AND xno=?1 AND direction=?2",
        rusqlite::params![xno, dir],
        |r| r.get(0),
    )?;
    let pts = rm_core::polyline::decode(&enc)?;
    eprintln!("pts={} segs={}", pts.len(), g.segs.len());
    // candidate counts per point
    let em = NewsonKrummEmission::default();
    let mut nocand = 0;
    for (i, p) in pts.iter().enumerate() {
        let n = rm_osm_match::viterbi::debug_candidates(&g, *p, &em);
        if n == 0 {
            nocand += 1;
            if nocand < 8 {
                eprintln!("pt{} no candidates at {:.5},{:.5}", i, p.lat, p.lon);
            }
        }
    }
    eprintln!("points with no candidates: {nocand}/{}", pts.len());
    let mut ctx = rm_osm_match::viterbi::Ctx::new(&g);
    let m = viterbi::match_path(&mut ctx, &pts, &em, &BasicTransition::default());
    eprintln!(
        "matched_pts={} runs={} path={:.0} matched={:.0} p50={:.1} p95={:.1} gaps={}",
        m.matched_points,
        m.runs.len(),
        m.path_length_m,
        m.matched_length_m,
        m.snap_p50_m,
        m.snap_p95_m,
        m.gap_repairs
    );
    Ok(())
}
