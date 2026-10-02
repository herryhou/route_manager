//! Raw capture parsing (serde structs mirror assets/Taichung/Taichung_API.md).

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug)]
pub struct Parsed {
    pub headers: Vec<RouteHeader>,
    pub providers: Vec<ProviderRow>,
    pub raws: Vec<RawRoute>,
    pub main_side: Vec<(String, String)>,
    pub joint: Vec<JointRow>,
    pub files: Vec<FileAudit>,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct RouteHeader {
    pub xno: String,
    pub route_code: String,
    pub departure: String,
    pub destination: String,
    pub description: String,
    pub seq: i64,
    pub is_cycled: bool,
    pub op_type: i64,
    pub provider_codes: Vec<String>,
}

#[derive(Debug)]
pub struct ProviderRow {
    pub code: String,
    pub name: String,
    pub telephone: String,
    pub op_type: i64,
}

#[derive(Debug)]
pub struct RawRoute {
    pub xno: String,
    pub stops: Vec<StopEdge>,
    pub poly_go: String,
    pub poly_back: String,
    pub provider_codes: Vec<String>,
}

#[derive(Debug)]
pub struct StopEdge {
    pub direction: i64,
    pub order_no: i64,
    pub ivr_no: Option<i64>,
    pub sid: i64,
    pub node_id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug)]
pub struct JointRow {
    pub bus_provider: String,
    pub provider_code: String,
    pub route_xno: String,
}

#[derive(Debug)]
pub struct FileAudit {
    pub kind: &'static str,
    pub name: String,
    pub sha256: String,
    pub row_count: usize,
}

fn read_file(path: &Path) -> Result<(String, Vec<u8>)> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let text =
        String::from_utf8(bytes.clone()).with_context(|| format!("utf8 {}", path.display()))?;
    Ok((text, bytes))
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

// ---- serde shapes ----

#[derive(Deserialize)]
struct GraphqlEdges<T> {
    edges: Vec<Wrap<T>>,
}
#[derive(Deserialize)]
struct Wrap<T> {
    node: T,
}
/// stations edges carry goBack/orderNo/sid/node directly (no relay wrap).
#[derive(Deserialize)]
struct StationEdges {
    edges: Vec<StationEdgeNode>,
}

#[derive(Deserialize)]
struct RoutesDoc {
    data: RoutesData,
}
#[derive(Deserialize)]
struct RoutesData {
    routes: GraphqlEdges<RouteNode>,
}
#[derive(Deserialize)]
struct RouteNode {
    id: String,
    name: String,
    seq: i64,
    departure: String,
    destination: String,
    description: String,
    #[serde(rename = "isCycled")]
    is_cycled: bool,
    #[serde(rename = "opType")]
    op_type: i64,
    providers: Option<GraphqlEdges<ProviderRef>>,
}
#[derive(Deserialize)]
struct ProviderRef {
    id: String,
}

#[derive(Deserialize)]
struct ProvidersDoc {
    data: ProvidersData,
}
#[derive(Deserialize)]
struct ProvidersData {
    providers: GraphqlEdges<ProviderNode>,
}
#[derive(Deserialize)]
struct ProviderNode {
    id: String,
    name: String,
    telephone: String,
    #[serde(rename = "opType")]
    op_type: i64,
}

#[derive(Deserialize)]
struct RouteDoc {
    data: RouteData,
}
#[derive(Deserialize)]
struct RouteData {
    route: Option<RouteBody>,
}
#[derive(Deserialize)]
struct RouteBody {
    stations: StationEdges,
    #[serde(rename = "routePoint")]
    route_point: Option<RoutePoint>,
    providers: Option<GraphqlEdges<ProviderRef>>,
}
#[derive(Deserialize)]
struct RoutePoint {
    go: Option<String>,
    back: Option<String>,
}
#[derive(Deserialize)]
struct StationEdgeNode {
    #[serde(rename = "goBack")]
    go_back: i64,
    #[serde(rename = "orderNo")]
    order_no: i64,
    #[serde(rename = "ivrNo")]
    ivr_no: Option<i64>,
    sid: i64,
    node: StationNode,
}
#[derive(Deserialize)]
struct StationNode {
    id: String,
    name: String,
    lat: f64,
    lon: f64,
}

#[derive(Deserialize)]
struct MainSideDoc {
    xno: serde_json::Value,
    #[serde(default)]
    side_routes: Option<String>,
    #[serde(default)]
    children: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize)]
struct JointDoc {
    data: JointData,
}
#[derive(Deserialize)]
struct JointData {
    #[serde(rename = "jointOperationsBuses")]
    joint_operations_buses: Vec<JointNode>,
}
#[derive(Deserialize)]
struct JointNode {
    #[serde(rename = "busProvider")]
    bus_provider: String,
    #[serde(rename = "providerId")]
    provider_id: serde_json::Value,
    #[serde(rename = "routeId")]
    route_id: serde_json::Value,
}

fn xno_sort_key(s: &str) -> (u8, i64, String) {
    match s.parse::<i64>() {
        Ok(n) => (0, n, s.to_string()),
        Err(_) => (1, i64::MAX, s.to_string()),
    }
}

/// Parse the whole capture tree. Order: headers/raws sorted by numeric xno.
pub fn parse_dir(source: &Path) -> Result<Parsed> {
    let mut p = Parsed {
        headers: Vec::new(),
        providers: Vec::new(),
        raws: Vec::new(),
        main_side: Vec::new(),
        joint: Vec::new(),
        files: Vec::new(),
        warnings: Vec::new(),
    };

    // providers
    let path = source.join("providers.json");
    let (text, bytes) = read_file(&path)?;
    let doc: ProvidersDoc = serde_json::from_str(&text).with_context(|| "providers.json")?;
    p.providers = doc
        .data
        .providers
        .edges
        .into_iter()
        .map(|w| ProviderRow {
            code: w.node.id,
            name: w.node.name,
            telephone: w.node.telephone,
            op_type: w.node.op_type,
        })
        .collect();
    p.files.push(FileAudit {
        kind: "providers",
        name: "providers.json".into(),
        sha256: sha256_hex(&bytes),
        row_count: p.providers.len(),
    });

    // route headers
    let path = source.join("routes.json");
    let (text, bytes) = read_file(&path)?;
    let doc: RoutesDoc = serde_json::from_str(&text).with_context(|| "routes.json")?;
    p.headers = doc
        .data
        .routes
        .edges
        .into_iter()
        .map(|w| RouteHeader {
            xno: w.node.id,
            route_code: w.node.name,
            departure: w.node.departure,
            destination: w.node.destination,
            description: w.node.description,
            seq: w.node.seq,
            is_cycled: w.node.is_cycled,
            op_type: w.node.op_type,
            provider_codes: w
                .node
                .providers
                .map(|pr| pr.edges.into_iter().map(|r| r.node.id).collect())
                .unwrap_or_default(),
        })
        .collect();
    p.headers
        .sort_by(|a, b| xno_sort_key(&a.xno).cmp(&xno_sort_key(&b.xno)));
    p.files.push(FileAudit {
        kind: "route_header",
        name: "routes.json".into(),
        sha256: sha256_hex(&bytes),
        row_count: p.headers.len(),
    });

    // raw route details, numeric xno order
    let mut raw_paths: Vec<(String, std::path::PathBuf)> = fs::read_dir(source.join("raw"))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name().into_string().unwrap_or_default();
            n.starts_with("route_") && n.ends_with(".json")
        })
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            let xno = n
                .trim_start_matches("route_")
                .trim_end_matches(".json")
                .to_string();
            Some((xno, e.path()))
        })
        .collect();
    raw_paths.sort_by(|a, b| xno_sort_key(&a.0).cmp(&xno_sort_key(&b.0)));

    for (xno, path) in raw_paths {
        let (text, bytes) = read_file(&path)?;
        let doc: RouteDoc =
            serde_json::from_str(&text).with_context(|| format!("raw/route_{xno}.json"))?;
        let Some(body) = doc.data.route else {
            anyhow::bail!("raw/route_{xno}.json: data.route is null");
        };
        let rp = body.route_point.unwrap_or(RoutePoint {
            go: None,
            back: None,
        });
        let stops = body
            .stations
            .edges
            .into_iter()
            .map(|e| -> Result<StopEdge> {
                Ok(StopEdge {
                    direction: match e.go_back {
                        1 => 0,
                        2 => 1,
                        other => anyhow::bail!("route {xno}: unexpected goBack {other}"),
                    },
                    order_no: e.order_no,
                    ivr_no: e.ivr_no,
                    sid: e.sid,
                    node_id: e.node.id,
                    name: e.node.name,
                    lat: e.node.lat,
                    lon: e.node.lon,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        p.raws.push(RawRoute {
            xno: xno.clone(),
            stops,
            poly_go: rp.go.unwrap_or_default(),
            poly_back: rp.back.unwrap_or_default(),
            provider_codes: body
                .providers
                .map(|pr| pr.edges.into_iter().map(|r| r.node.id).collect())
                .unwrap_or_default(),
        });
        p.files.push(FileAudit {
            kind: "route_detail",
            name: format!("raw/route_{xno}.json"),
            sha256: sha256_hex(&bytes),
            row_count: 0, // stop edges counted by caller rows
        });
    }

    // main-side-routes: children preferred, sideRoutes CSV fallback
    let path = source.join("main-side-routes.json");
    let (text, bytes) = read_file(&path)?;
    let docs: Vec<MainSideDoc> =
        serde_json::from_str(&text).with_context(|| "main-side-routes.json")?;
    let mut pairs = Vec::new();
    for d in &docs {
        let main = match &d.xno {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            other => anyhow::bail!("main-side-routes: bad xno {other}"),
        };
        let mut sides: Vec<String> = d
            .children
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|v| match v {
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::String(s) => s.clone(),
                other => format!("{other}"),
            })
            .collect();
        if sides.is_empty() {
            if let Some(csv) = &d.side_routes {
                sides = csv
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect();
            }
        }
        for s in sides {
            pairs.push((main.clone(), s));
        }
    }
    pairs.sort();
    pairs.dedup();
    p.main_side = pairs;
    p.files.push(FileAudit {
        kind: "main_side_route",
        name: "main-side-routes.json".into(),
        sha256: sha256_hex(&bytes),
        row_count: p.main_side.len(),
    });

    // jointOperationsBuses (optional)
    let path = source.join("jointOperationsBuses.json");
    if path.exists() {
        let (text, bytes) = read_file(&path)?;
        let doc: JointDoc =
            serde_json::from_str(&text).with_context(|| "jointOperationsBuses.json")?;
        p.joint = doc
            .data
            .joint_operations_buses
            .into_iter()
            .map(|n| JointRow {
                bus_provider: n.bus_provider,
                provider_code: n.provider_id.to_string().trim_matches('"').to_string(),
                route_xno: n.route_id.to_string().trim_matches('"').to_string(),
            })
            .collect();
        p.files.push(FileAudit {
            kind: "joint_operation",
            name: "jointOperationsBuses.json".into(),
            sha256: sha256_hex(&bytes),
            row_count: p.joint.len(),
        });
    }

    // warnings: header<->raw mismatches + dangling main-side refs
    let header_xnos: std::collections::HashSet<&str> =
        p.headers.iter().map(|h| h.xno.as_str()).collect();
    let raw_xnos: std::collections::HashSet<&str> = p.raws.iter().map(|r| r.xno.as_str()).collect();
    for h in &p.headers {
        if !raw_xnos.contains(h.xno.as_str()) {
            p.warnings
                .push(format!("route header without raw detail: xno={}", h.xno));
        }
    }
    for r in &p.raws {
        if !header_xnos.contains(r.xno.as_str()) {
            p.warnings
                .push(format!("raw detail without route header: xno={}", r.xno));
        }
    }
    for (main, side) in &p.main_side {
        for (role, xno) in [("main", main), ("side", side)] {
            if !header_xnos.contains(xno.as_str()) {
                p.warnings
                    .push(format!("main-side ref to unknown route: {role} xno={xno}"));
            }
        }
    }
    // raw provider vs header provider mismatch
    for r in &p.raws {
        if let Some(h) = p.headers.iter().find(|h| h.xno == r.xno) {
            let hs: std::collections::HashSet<&str> =
                h.provider_codes.iter().map(String::as_str).collect();
            let rs: std::collections::HashSet<&str> =
                r.provider_codes.iter().map(String::as_str).collect();
            if hs != rs {
                p.warnings.push(format!(
                    "provider mismatch: xno={} headers={:?} raw={:?}",
                    r.xno, h.provider_codes, r.provider_codes
                ));
            }
        }
    }
    p.warnings.sort();
    p.warnings.dedup();
    Ok(p)
}
