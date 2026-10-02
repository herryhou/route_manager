//! Shared capture fixture builder (feature-gated for other crates' tests).
//!
//! Mirrors real capture shapes from assets/Taichung/Taichung_API.md:
//! 2 routes; nodes 44/46 same name 28 m apart; route 1 both directions,
//! route 2 single direction; side ref 999 dangling.

use std::fs;
use std::path::Path;

pub fn write(dir: &Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

pub fn routes_json() -> &'static str {
    r#"{"data":{"routes":{"edges":[
        {"node":{"id":"1","name":"1","seq":1,"departure":"A","destination":"B","description":"A - B","isCycled":true,"opType":0,
            "providers":{"edges":[{"node":{"id":"5","name":"台中客運","telephone":"04-2222"}}]}}},
        {"node":{"id":"2","name":"2","seq":2,"departure":"C","destination":"D","description":"C - D","isCycled":false,"opType":0,
            "providers":{"edges":[]}}}
    ]}}}"#
}

pub fn make_fixture(dir: &Path) {
    write(dir, "routes.json", routes_json());
    write(
        dir,
        "providers.json",
        r#"{"data":{"providers":{"edges":[
            {"node":{"id":"5","name":"台中客運","telephone":"04-2222","opType":0}}
        ]}}}"#,
    );
    write(
        dir,
        "main-side-routes.json",
        r#"[{"xno":1,"sideRoutes":"2,999","children":[2,999],"_id":"m1","createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z","__v":0}]"#,
    );
    write(
        dir,
        "jointOperationsBuses.json",
        r#"{"data":{"jointOperationsBuses":[]}}"#,
    );
    write(
        dir,
        "raw/route_1.json",
        r#"{"data":{"route":{
            "stations":{"edges":[
                {"goBack":1,"orderNo":1,"ivrNo":101,"sid":11,"node":{"id":"44","name":"干城站","lat":24.1409,"lon":120.6857}},
                {"goBack":1,"orderNo":2,"ivrNo":102,"sid":12,"node":{"id":"46","name":"干城站","lat":24.1408,"lon":120.6859}},
                {"goBack":2,"orderNo":1,"ivrNo":201,"sid":13,"node":{"id":"46","name":"干城站","lat":24.1408,"lon":120.6859}}
            ]},
            "routePoint":{"go":"_p~iF~ps|U","back":null},
            "providers":{"edges":[{"node":{"id":"5","name":"台中客運","opType":0}}]}
        }}}"#,
    );
    write(
        dir,
        "raw/route_2.json",
        r#"{"data":{"route":{
            "stations":{"edges":[
                {"goBack":1,"orderNo":5,"ivrNo":null,"sid":21,"node":{"id":"44","name":"干城站","lat":24.1409,"lon":120.6857}}
            ]},
            "routePoint":null,
            "providers":{"edges":[]}
        }}}"#,
    );
}
