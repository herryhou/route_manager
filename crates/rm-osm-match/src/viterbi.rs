//! Viterbi matcher over the candidate lattice: path points -> directed way
//! runs with cumulative chainage along the path.

use std::collections::HashMap;

use crate::graph::RoadGraph;
use crate::models::{EmissionModel, TransitionCtx, TransitionModel};
use rm_core::LatLng;

/// A contiguous run of one way, in travel order, with the path chainage
/// span it covers.
#[derive(Debug, Clone, PartialEq)]
pub struct WayRun {
    pub way_id: i64,
    pub entry_node: i64, // osm node ids
    pub exit_node: i64,
    pub forward: bool, // travel direction matches the way's node order
    pub cum_start_m: f64,
    pub cum_end_m: f64,
}

#[derive(Debug, Default, Clone)]
pub struct MatchOutput {
    pub runs: Vec<WayRun>,
    pub matched_points: usize,
    pub path_length_m: f64,
    pub matched_length_m: f64,
    pub snap_p50_m: f64,
    pub snap_p95_m: f64,
    pub snap_max_m: f64,
    pub outliers: usize, // matched points snapping worse than OUTLIER_M
    pub gap_repairs: usize,
}

/// snap distance beyond which a matched point counts as an outlier
/// (mirrors rm-segments' default --outlier-m)
pub const OUTLIER_M: f64 = 50.0;

/// A candidate state: a directed position on one segment.
#[derive(Clone, Copy)]
struct State {
    seg: u32,
    forward: bool, // traveling a -> b when true
    t: f64,        // position along a->b, 0..=1
    dist_m: f64,   // emission distance
}

const MOTION_TOLERANCE_M: f64 = 5.0; // reverse jitter forgiven on same state
const ASTAR_EXPANSION_CAP: usize = 4000;

/// cached A* result: network length + directed segs traversed
type AstarPath = Option<(f64, Vec<(u32, bool)>)>;

struct Ctx<'a> {
    g: &'a RoadGraph,
    astar_cache: HashMap<(u32, u32), AstarPath>,
}

fn entry_of(g: &RoadGraph, s: &State) -> u32 {
    let seg = g.seg(s.seg);
    if s.forward {
        seg.a
    } else {
        seg.b
    }
}

fn exit_of(g: &RoadGraph, s: &State) -> u32 {
    let seg = g.seg(s.seg);
    if s.forward {
        seg.b
    } else {
        seg.a
    }
}

fn along_to_exit(g: &RoadGraph, s: &State) -> f64 {
    let seg = g.seg(s.seg);
    (if s.forward { 1.0 - s.t } else { s.t }) * seg.length_m
}

fn along_from_entry(g: &RoadGraph, s: &State) -> f64 {
    let seg = g.seg(s.seg);
    (if s.forward { s.t } else { 1.0 - s.t }) * seg.length_m
}

/// candidate count at `p` (debug aid for examples/dbg_match.rs)
pub fn debug_candidates(g: &RoadGraph, p: LatLng, em: &dyn EmissionModel) -> usize {
    candidates(g, p, em).len()
}

fn candidates(g: &RoadGraph, p: LatLng, em: &dyn EmissionModel) -> Vec<State> {
    let radius = em.search_radius_m();
    let mut out = Vec::new();
    for idx in g.segments_near(p, radius) {
        let seg = g.seg(idx);
        let (proj, dist) =
            rm_core::geo::point_to_segment_m(p, g.node_pos(seg.a), g.node_pos(seg.b));
        if dist > radius {
            continue;
        }
        let len = seg.length_m.max(1e-9);
        let t = (rm_core::geo::haversine_m(g.node_pos(seg.a), proj) / len).clamp(0.0, 1.0);
        // directed states honoring oneway
        if seg.oneway != -1 {
            out.push(State {
                seg: idx,
                forward: true,
                t,
                dist_m: dist,
            });
        }
        if seg.oneway != 1 {
            out.push(State {
                seg: idx,
                forward: false,
                t,
                dist_m: dist,
            });
        }
    }
    out
}

/// unit direction of a state's travel (degree space, lon scaled)
fn dir_vec(g: &RoadGraph, s: &State) -> (f64, f64) {
    let seg = g.seg(s.seg);
    let a = g.node_pos(seg.a);
    let b = g.node_pos(seg.b);
    let (dlat, dlon) = (b.lat - a.lat, (b.lon - a.lon) * 0.913); // cos(24.1 deg)
    let (dlat, dlon) = if s.forward {
        (dlat, dlon)
    } else {
        (-dlat, -dlon)
    };
    let n = (dlat * dlat + dlon * dlon).sqrt().max(1e-12);
    (dlat / n, dlon / n)
}

fn turn_cos(g: &RoadGraph, s1: &State, s2: &State) -> f64 {
    let (x1, y1) = dir_vec(g, s1);
    let (x2, y2) = dir_vec(g, s2);
    (x1 * x2 + y1 * y2).clamp(-1.0, 1.0)
}

/// A* over directed adjacency (oneway honored); returns length + seg path.
fn astar(ctx: &mut Ctx, start: u32, goal: u32) -> Option<(f64, Vec<(u32, bool)>)> {
    use std::cmp::Reverse;
    if let Some(v) = ctx.astar_cache.get(&(start, goal)) {
        return v.clone();
    }
    let g = ctx.g;
    let goal_pos = g.node_pos(goal);
    let mut dist: HashMap<u32, f64> = HashMap::new();
    let mut came: HashMap<u32, (u32, u32, bool)> = HashMap::new(); // node -> (prev, seg, forward)
                                                                   // f64 is not Ord; distances are non-negative so bit patterns order correctly
    let mut heap: std::collections::BinaryHeap<Reverse<(u64, u32)>> =
        std::collections::BinaryHeap::new();
    dist.insert(start, 0.0);
    heap.push(Reverse((0.0f64.to_bits(), start)));
    let mut expansions = 0;
    let mut found = false;
    while let Some(Reverse((_, u))) = heap.pop() {
        if u == goal {
            found = true;
            break;
        }
        expansions += 1;
        if expansions > ASTAR_EXPANSION_CAP {
            break;
        }
        let du = *dist.get(&u)?;
        for &(v, seg_idx) in &g.out_edges[u as usize] {
            let seg = g.seg(seg_idx);
            let nd = du + seg.length_m;
            if nd < *dist.get(&v).unwrap_or(&f64::INFINITY) {
                let forward = seg.a == u;
                dist.insert(v, nd);
                came.insert(v, (u, seg_idx, forward));
                let h = rm_core::geo::haversine_m(g.node_pos(v), goal_pos);
                heap.push(Reverse(((nd + h).to_bits(), v)));
            }
        }
    }
    let result = if found {
        // unwind
        let mut segs_rev = Vec::new();
        let mut cur = goal;
        while cur != start {
            let (prev, seg_idx, forward) = came[&cur];
            segs_rev.push((seg_idx, forward));
            cur = prev;
        }
        segs_rev.reverse();
        Some((dist[&goal], segs_rev))
    } else {
        None
    };
    ctx.astar_cache.insert((start, goal), result.clone());
    result
}

struct NetStep {
    net_m: f64,
    uturn: bool,
    turn_cos: f64,
    multi_hop: bool,
    /// full directed segs traversed between exit(s1) and entry(s2)
    intermediate: Vec<(u32, bool)>,
}

/// Max plausible network detour for a repair: base allowance plus a factor
/// on the gps step. Beyond it the transition is treated as disconnected
/// (keeps A* from wandering km-scale loops through coverage holes).
fn repair_bound(gps_m: f64) -> f64 {
    500.0 + 2.0 * gps_m
}

/// Network traversal between two consecutive states. None = no connection.
/// `gps_m` is the great-circle step between the two path points.
#[allow(clippy::too_many_arguments)]
fn evaluate(ctx: &mut Ctx, s1: &State, s2: &State, gps_m: f64) -> Option<NetStep> {
    let g = ctx.g;
    if s1.seg == s2.seg {
        let len = g.seg(s1.seg).length_m;
        let motion = (s2.t - s1.t) * len;
        if s1.forward == s2.forward {
            return Some(NetStep {
                net_m: motion.abs(),
                uturn: motion < -MOTION_TOLERANCE_M,
                turn_cos: 1.0,
                multi_hop: false,
                intermediate: Vec::new(),
            });
        }
        // opposite directions on the same segment: U-turn through an end
        let exit1 = exit_of(g, s1);
        let entry2 = entry_of(g, s2);
        if exit1 != entry2 {
            return None; // would need to pass through the segment itself
        }
        return Some(NetStep {
            net_m: along_to_exit(g, s1) + along_from_entry(g, s2),
            uturn: true,
            turn_cos: -1.0,
            multi_hop: false,
            intermediate: Vec::new(),
        });
    }
    let exit1 = exit_of(g, s1);
    let entry2 = entry_of(g, s2);
    if exit1 == entry2 {
        return Some(NetStep {
            net_m: along_to_exit(g, s1) + along_from_entry(g, s2),
            uturn: false,
            turn_cos: turn_cos(g, s1, s2),
            multi_hop: false,
            intermediate: Vec::new(),
        });
    }
    // straight-line span already exceeds any plausible detour: reject
    // before paying for A*
    let span = rm_core::geo::haversine_m(g.node_pos(exit1), g.node_pos(entry2));
    if span > repair_bound(gps_m) {
        return None;
    }
    let (astar_len, astar_segs) = astar(ctx, exit1, entry2)?;
    let net = along_to_exit(g, s1) + astar_len + along_from_entry(g, s2);
    if net > repair_bound(gps_m) {
        return None;
    }
    Some(NetStep {
        net_m: net,
        uturn: false,
        turn_cos: 1.0,
        multi_hop: !astar_segs.is_empty(),
        intermediate: astar_segs,
    })
}

#[derive(Clone, Copy)]
struct Piece {
    seg: u32,
    way_id: i64,
    forward: bool,
    entry_node: i64, // osm ids
    exit_node: i64,
    cs: f64,
    ce: f64,
}

/// Match `pts` (a decoded direction polyline) onto `graph`.
pub fn match_path(
    graph: &RoadGraph,
    pts: &[LatLng],
    em: &dyn EmissionModel,
    tm: &dyn TransitionModel,
) -> MatchOutput {
    let mut out = MatchOutput {
        path_length_m: rm_core::geo::path_length_m(pts),
        ..Default::default()
    };
    if pts.len() < 2 {
        return out;
    }
    let cum: Vec<f64> = {
        let mut c = vec![0.0];
        for i in 1..pts.len() {
            c.push(c[i - 1] + rm_core::geo::haversine_m(pts[i - 1], pts[i]));
        }
        c
    };

    let mut states_per_pt: Vec<Vec<State>> = Vec::with_capacity(pts.len());
    let mut matched: Vec<usize> = Vec::new();
    for (i, p) in pts.iter().enumerate() {
        let st = candidates(graph, *p, em);
        if st.is_empty() {
            states_per_pt.push(Vec::new());
        } else {
            states_per_pt.push(st);
            matched.push(i);
        }
    }
    out.matched_points = matched.len();
    if matched.len() < 2 {
        return out;
    }

    let mut ctx = Ctx {
        g: graph,
        astar_cache: HashMap::new(),
    };

    // DP
    let mut dp: Vec<Vec<f64>> = Vec::with_capacity(matched.len());
    let mut parent: Vec<Vec<i32>> = Vec::with_capacity(matched.len());
    dp.push(
        states_per_pt[matched[0]]
            .iter()
            .map(|s| em.emission_cost(s.dist_m))
            .collect(),
    );
    parent.push(vec![-1; states_per_pt[matched[0]].len()]);
    for w in 1..matched.len() {
        let i_prev = matched[w - 1];
        let i = matched[w];
        // Newson-Krumm difference term uses consecutive-point displacement,
        // not the accumulated path length: unmatched points between the two
        // would otherwise inflate gps and reward detouring network paths.
        let gps = rm_core::geo::haversine_m(pts[i_prev], pts[i]);
        let prev = &states_per_pt[i_prev];
        let cur = &states_per_pt[i];
        let mut row = vec![f64::INFINITY; cur.len()];
        let mut prow = vec![-1i32; cur.len()];
        for (k, s2) in cur.iter().enumerate() {
            let e2 = em.emission_cost(s2.dist_m);
            for (j, s1) in prev.iter().enumerate() {
                let base = dp[w - 1][j];
                if !base.is_finite() {
                    continue;
                }
                let Some(step) = evaluate(&mut ctx, s1, s2, gps) else {
                    continue;
                };
                let c = base
                    + e2
                    + tm.transition_cost(&TransitionCtx {
                        gps_m: gps,
                        net_m: step.net_m,
                        uturn: step.uturn,
                        turn_cos: step.turn_cos,
                        multi_hop: step.multi_hop,
                    });
                if c < row[k] {
                    row[k] = c;
                    prow[k] = j as i32;
                }
            }
        }
        dp.push(row);
        parent.push(prow);
        // if no state at w is reachable from w-1 (disconnected jump), restart
        // the DP here: emissions only, no parent. One unbridgeable transition
        // must not zero out the whole direction.
        if dp[w].iter().all(|c| !c.is_finite()) {
            for (k, s2) in states_per_pt[matched[w]].iter().enumerate() {
                dp[w][k] = em.emission_cost(s2.dist_m);
                parent[w][k] = -1;
            }
        }
    }

    // backtrace, one DP segment at a time (segments split at restart rows)
    let last = dp.len() - 1;
    let mut chosen: Vec<usize> = vec![0; matched.len()];
    let mut end = last;
    loop {
        let j = (0..dp[end].len())
            .min_by(|&a, &b| dp[end][a].total_cmp(&dp[end][b]))
            .expect("row has a finite entry (restart guarantees one)");
        let mut w = end;
        let mut cur = j;
        chosen[w] = cur;
        while w > 0 && parent[w][cur] >= 0 {
            cur = parent[w][cur] as usize;
            w -= 1;
            chosen[w] = cur;
        }
        if w == 0 {
            break;
        }
        // w is a restart row already assigned; continue with the segment above
        end = w - 1;
    }

    // snap stats over chosen states
    let mut snaps: Vec<f64> = (0..matched.len())
        .map(|w| states_per_pt[matched[w]][chosen[w]].dist_m)
        .collect();
    snaps.sort_by(|a, b| a.total_cmp(b));
    let n = snaps.len();
    out.snap_p50_m = snaps[n * 50 / 100];
    out.snap_p95_m = snaps[(n * 95 / 100).min(n - 1)];
    out.snap_max_m = *snaps.last().unwrap();
    out.outliers = snaps.iter().filter(|&&d| d > OUTLIER_M).count();

    // extraction: pieces per transition, then merge
    let mut pieces: Vec<Piece> = Vec::new();
    for w in 1..matched.len() {
        let i_prev = matched[w - 1];
        let i = matched[w];
        let s1 = states_per_pt[i_prev][chosen[w - 1]];
        let s2 = states_per_pt[i][chosen[w]];
        // same displacement gps the DP judged this transition by
        let gps_step = rm_core::geo::haversine_m(pts[i_prev], pts[i]);
        let Some(step) = evaluate(&mut ctx, &s1, &s2, gps_step) else {
            continue;
        };
        if step.multi_hop {
            out.gap_repairs += 1;
        }
        if step.net_m <= 1e-9 {
            continue;
        }
        let c0 = cum[i_prev];
        let gps = cum[i] - c0;
        let mut parts: Vec<(State, f64)> = Vec::new();
        if s1.seg == s2.seg && s1.forward == s2.forward {
            // direct motion within one segment: span t1..t2 (may be interior)
            let plen = (s2.t - s1.t).abs() * ctx.g.seg(s1.seg).length_m;
            parts.push((s1, plen));
        } else {
            parts.push((s1, along_to_exit(ctx.g, &s1)));
            for (seg_idx, forward) in &step.intermediate {
                let st = State {
                    seg: *seg_idx,
                    forward: *forward,
                    t: if *forward { 0.0 } else { 1.0 },
                    dist_m: 0.0,
                };
                parts.push((st, ctx.g.seg(*seg_idx).length_m));
            }
            parts.push((s2, along_from_entry(ctx.g, &s2)));
        }
        let mut n0 = 0.0f64;
        for (st, plen) in parts {
            if plen <= 1e-9 {
                continue;
            }
            let cs = c0 + gps * n0 / step.net_m;
            let ce = c0 + gps * (n0 + plen) / step.net_m;
            pieces.push(Piece {
                seg: st.seg,
                way_id: ctx.g.seg(st.seg).way_id,
                forward: st.forward,
                entry_node: ctx.g.node_osm_id(entry_of(ctx.g, &st)),
                exit_node: ctx.g.node_osm_id(exit_of(ctx.g, &st)),
                cs,
                ce,
            });
            n0 += plen;
        }
    }

    // merge contiguous pieces of the same way+direction; interior
    // same-segment spans chain without a shared boundary node
    let mut runs: Vec<WayRun> = Vec::new();
    let mut last_seg: Option<u32> = None;
    for p in pieces {
        if let Some(last) = runs.last_mut() {
            let contig = (p.cs - last.cum_end_m).abs() < 1e-6;
            let connected = last.exit_node == p.entry_node || last_seg == Some(p.seg);
            if contig && connected && last.way_id == p.way_id && last.forward == p.forward {
                last.cum_end_m = p.ce;
                last.exit_node = p.exit_node;
                last_seg = Some(p.seg);
                continue;
            }
        }
        last_seg = Some(p.seg);
        runs.push(WayRun {
            way_id: p.way_id,
            // seg not carried into the run; entry/exit nodes suffice downstream
            entry_node: p.entry_node,
            exit_node: p.exit_node,
            forward: p.forward,
            cum_start_m: p.cs,
            cum_end_m: p.ce,
        });
    }
    out.matched_length_m = runs.iter().map(|r| r.cum_end_m - r.cum_start_m).sum();
    out.runs = runs;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{NodePos, RoadGraph, WayInput};
    use crate::models::{BasicTransition, NewsonKrummEmission};

    const D: f64 = 0.001; // ~111 m

    fn node(id: i64, r: i64, c: i64) -> NodePos {
        NodePos {
            osm_id: id,
            lat: 24.1400 + r as f64 * D,
            lon: 120.6800 + c as f64 * D,
        }
    }

    fn run_match(g: &RoadGraph, pts: &[LatLng]) -> MatchOutput {
        match_path(
            g,
            pts,
            &NewsonKrummEmission::default(),
            &BasicTransition::default(),
        )
    }

    /// straight two-segment way: 1 - 2 - 3 south to north
    #[test]
    fn straight_path_merges_into_one_way_run() {
        let g = RoadGraph::build(
            vec![node(1, 0, 0), node(2, 1, 0), node(3, 2, 0)],
            &[WayInput {
                way_id: 100,
                oneway: 0,
                node_ids: vec![1, 2, 3],
            }],
        );
        let pts = vec![
            LatLng {
                lat: 24.1400,
                lon: 120.6800,
            },
            LatLng {
                lat: 24.1400 + D / 2.0,
                lon: 120.6800,
            },
            LatLng {
                lat: 24.1400 + D,
                lon: 120.6800,
            },
            LatLng {
                lat: 24.1400 + 3.0 * D / 2.0,
                lon: 120.6800,
            },
            LatLng {
                lat: 24.1400 + 2.0 * D,
                lon: 120.6800,
            },
        ];
        let out = run_match(&g, &pts);
        assert_eq!(out.runs.len(), 1, "{:?}", out.runs);
        let r = &out.runs[0];
        assert_eq!((r.way_id, r.entry_node, r.exit_node), (100, 1, 3));
        assert!(r.cum_start_m < 5.0, "{}", r.cum_start_m);
        let total = 2.0 * 111.3;
        assert!(
            (r.cum_end_m - total).abs() < 5.0,
            "cum_end {} want ~{total}",
            r.cum_end_m
        );
        assert!((out.matched_length_m - total).abs() < 5.0);
        assert_eq!(out.gap_repairs, 0);
        assert!(out.snap_max_m < 1.0);
    }

    /// L-shape across two ways sharing node 2
    #[test]
    fn turn_across_ways_yields_two_runs() {
        let g = RoadGraph::build(
            vec![node(1, 0, 0), node(2, 1, 0), node(4, 1, -1), node(5, 1, 1)],
            &[
                WayInput {
                    way_id: 100,
                    oneway: 0,
                    node_ids: vec![1, 2],
                },
                WayInput {
                    way_id: 200,
                    oneway: 0,
                    node_ids: vec![4, 2, 5],
                },
            ],
        );
        let pts = vec![
            LatLng {
                lat: 24.1400,
                lon: 120.6800,
            },
            LatLng {
                lat: 24.1400 + D,
                lon: 120.6800,
            },
            LatLng {
                lat: 24.1400 + D,
                lon: 120.6800 + D,
            },
        ];
        let out = run_match(&g, &pts);
        assert_eq!(out.runs.len(), 2, "{:?}", out.runs);
        assert_eq!(
            (
                out.runs[0].way_id,
                out.runs[0].entry_node,
                out.runs[0].exit_node
            ),
            (100, 1, 2)
        );
        assert_eq!(
            (
                out.runs[1].way_id,
                out.runs[1].entry_node,
                out.runs[1].exit_node
            ),
            (200, 2, 5)
        );
        assert!(out.runs[0].cum_end_m <= out.runs[1].cum_start_m + 1e-6);
    }

    /// path against an oneway street with a parallel bidirectional street:
    /// matcher must pick the legal one
    #[test]
    fn oneway_reverse_is_avoided_when_alternative_exists() {
        let g = RoadGraph::build(
            vec![
                node(1, 0, 0),
                node(2, 1, 0),
                node(11, 0, 1), // parallel way nodes, one block east
                node(12, 1, 1),
            ],
            &[
                WayInput {
                    way_id: 300,
                    oneway: 1,
                    node_ids: vec![1, 2],
                }, // northbound only
                WayInput {
                    way_id: 400,
                    oneway: 0,
                    node_ids: vec![11, 12],
                },
            ],
        );
        // path travels SOUTH on the oneway corridor (offset slightly east so
        // the parallel way is nearer)
        let pts = vec![
            LatLng {
                lat: 24.1400 + D,
                lon: 120.6800 + D * 0.55,
            },
            LatLng {
                lat: 24.1400 + D / 2.0,
                lon: 120.6800 + D * 0.55,
            },
            LatLng {
                lat: 24.1400,
                lon: 120.6800 + D * 0.55,
            },
        ];
        let out = run_match(&g, &pts);
        assert!(
            out.runs.iter().all(|r| r.way_id != 300),
            "oneway 300 travelled reverse: {:?}",
            out.runs
        );
        assert!(
            out.runs.iter().any(|r| r.way_id == 400),
            "parallel way 400 should be used: {:?}",
            out.runs
        );
    }

    /// disconnected corridor repaired through a connector chain (A*)
    #[test]
    fn gap_between_corridors_is_repaired_via_astar() {
        // 1 - 2   then gap   3 - 4 , connected by side street 2 - 5 - 3
        let g = RoadGraph::build(
            vec![
                node(1, 0, 0),
                node(2, 0, 1),
                node(3, 0, 3),
                node(4, 0, 4),
                node(5, 1, 2),
            ],
            &[
                WayInput {
                    way_id: 500,
                    oneway: 0,
                    node_ids: vec![1, 2],
                },
                WayInput {
                    way_id: 501,
                    oneway: 0,
                    node_ids: vec![3, 4],
                },
                WayInput {
                    way_id: 502,
                    oneway: 0,
                    node_ids: vec![2, 5, 3],
                },
            ],
        );
        let pts = vec![
            LatLng {
                lat: 24.1400,
                lon: 120.6800,
            },
            LatLng {
                lat: 24.1400,
                lon: 120.6800 + D,
            },
            // far off-network point: no candidates, forces a gap between the
            // two corridors that A* must bridge via way 502
            LatLng {
                lat: 24.1400 + 3.0 * D,
                lon: 120.6800 + 2.0 * D,
            },
            LatLng {
                lat: 24.1400,
                lon: 120.6800 + 3.0 * D,
            },
            LatLng {
                lat: 24.1400,
                lon: 120.6800 + 4.0 * D,
            },
        ];
        let out = run_match(&g, &pts);
        let ways: Vec<i64> = out.runs.iter().map(|r| r.way_id).collect();
        assert_eq!(ways, vec![500, 502, 501], "{:?}", out.runs);
        assert!(out.gap_repairs >= 1, "connector found via A*");
        assert!(out.matched_length_m > 3.0 * 111.0);
    }

    /// a long connecting chain exists but a small gps step cannot justify the
    /// detour: repair is rejected and the DP restarts instead of bridging
    #[test]
    fn disproportionate_bridge_is_rejected() {
        // way1 1-2, way2 31-32 close together in space, connected only by a
        // long chain around (2 -> ... -> 31) several km
        let g = RoadGraph::build(
            vec![
                node(1, 0, 0),
                node(2, 0, 1),
                node(31, 2, 0),
                node(32, 2, 1),
                node(41, 0, 10),
                node(42, 2, 10),
            ],
            &[
                WayInput {
                    way_id: 100,
                    oneway: 0,
                    node_ids: vec![1, 2],
                },
                WayInput {
                    way_id: 900,
                    oneway: 0,
                    node_ids: vec![31, 32],
                },
                // connector: 2 -> 41 -> 42 -> 31 (~2.2 km each leg)
                WayInput {
                    way_id: 800,
                    oneway: 0,
                    node_ids: vec![2, 41, 42, 31],
                },
            ],
        );
        let pts = vec![
            LatLng {
                lat: 24.1400,
                lon: 120.6800,
            },
            LatLng {
                lat: 24.1400,
                lon: 120.6800 + D,
            },
            // small displacement jump (~250 m) to way 900
            LatLng {
                lat: 24.1400 + 2.0 * D,
                lon: 120.6800,
            },
            LatLng {
                lat: 24.1400 + 2.0 * D,
                lon: 120.6800 + D,
            },
        ];
        let out = run_match(&g, &pts);
        let ways: Vec<i64> = out.runs.iter().map(|r| r.way_id).collect();
        assert_eq!(ways, vec![100, 900], "{:?}", out.runs);
        assert_eq!(
            out.gap_repairs, 0,
            "2.2 km detour must not repair a 250 m step"
        );
    }

    /// a path jumping between two disconnected road components still yields
    /// runs for both: one unbridgeable transition must not zero the match
    #[test]
    fn disconnected_jump_restarts_dp_not_empties_it() {
        let g = RoadGraph::build(
            vec![
                node(1, 0, 0),
                node(2, 1, 0),
                node(31, 0, 50),
                node(32, 1, 50), // ~5 km east, no connecting way
            ],
            &[
                WayInput {
                    way_id: 100,
                    oneway: 0,
                    node_ids: vec![1, 2],
                },
                WayInput {
                    way_id: 900,
                    oneway: 0,
                    node_ids: vec![31, 32],
                },
            ],
        );
        let pts = vec![
            LatLng {
                lat: 24.1400,
                lon: 120.6800,
            },
            LatLng {
                lat: 24.1400 + D,
                lon: 120.6800,
            },
            // jump to the far component
            LatLng {
                lat: 24.1400,
                lon: 120.6800 + 50.0 * D,
            },
            LatLng {
                lat: 24.1400 + D,
                lon: 120.6800 + 50.0 * D,
            },
        ];
        let out = run_match(&g, &pts);
        let ways: Vec<i64> = out.runs.iter().map(|r| r.way_id).collect();
        assert_eq!(ways, vec![100, 900], "{:?}", out.runs);
        assert_eq!(out.matched_points, 4);
    }

    /// empty path or path far from any way matches nothing, does not panic
    #[test]
    fn unmatched_path_is_reported_not_fatal() {
        let g = RoadGraph::build(
            vec![node(1, 0, 0), node(2, 1, 0)],
            &[WayInput {
                way_id: 1,
                oneway: 0,
                node_ids: vec![1, 2],
            }],
        );
        let out = run_match(&g, &[]);
        assert!(out.runs.is_empty());
        let far = vec![
            LatLng {
                lat: 24.2000,
                lon: 120.8000,
            },
            LatLng {
                lat: 24.2001,
                lon: 120.8001,
            },
        ];
        let out2 = run_match(&g, &far);
        assert!(out2.runs.is_empty());
        assert_eq!(out2.matched_points, 0);
    }
}
