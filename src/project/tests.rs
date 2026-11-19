use super::*;
use crate::hex;
use crate::snapshot::{self, parse_facts, Receipt};
use serde_json::json;
use std::path::Path;

fn golden(name: &str) -> Projection {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    project(&snapshot::read_dir(&dir).unwrap().records).unwrap()
}

/// Parse synthetic JSONL lines into records (through the real validator).
fn records(lines: &[Value]) -> Vec<Record> {
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let receipt: Receipt = serde_json::from_value(json!({
        "snapshot_id": format!("sha256:{}", "a".repeat(64)),
        "format_version": 1,
        "output_hashes": {"facts.jsonl": hex::sha256_tagged(body.as_bytes())},
        "fact_count": lines.len(),
    }))
    .unwrap();
    parse_facts(&receipt, body.as_bytes()).unwrap().1
}

fn id(n: u8) -> String {
    format!("{:032x}", n)
}

fn fact<'a>(p: &'a Projection, id: &str) -> &'a FactRow {
    p.facts
        .iter()
        .find(|f| f.enola_id == id)
        .unwrap_or_else(|| panic!("no fact {id}"))
}

fn group(p: &Projection, t: RelType, target: Target) -> &[EdgeRow] {
    p.edges
        .iter()
        .find(|g| g.rel_type == t && g.target == target)
        .map_or(&[], |g| &g.rows)
}

fn has_edge(p: &Projection, t: RelType, target: Target, src: &str, dst: &str) -> bool {
    group(p, t, target)
        .iter()
        .any(|e| e.src == src && e.dst == dst)
}

// Golden ids, from tests/fixtures/GENERATED.md.
const CLIENT: &str = "d371930757da8cc5882173de7414d30e";
const SERVER: &str = "8c598b0d43c049f2e4570f276c7ec3ff";
const HANDLER: &str = "a7c1e9812c4880ea811f1b2e7dab7c09";
const CLIENT_LIST: &str = "6fbc4f38a1bee68967da09f098cfda83";
const SERVER_LIST: &str = "95ec3bfae02e50b621f1013716cd960e";
const UNMATCHED: &str = "c2cbbdeff1bb4e4e0eff329ddd6a806d";
const FETCH_DYNAMIC: &str = "61abb2eadb894c23f31ffdad6f9c34bb";
const FETCH_ORDER: &str = "1cec1362aa13755762a4f1fc3d4cdce6";

#[test]
fn cross_repo_golden_tuple() {
    let p = golden("cross-repo-v0.5.3");
    // client route -> server route, by matched_routes id, with Enola's confidence
    let m = p.route_matches.iter().find(|m| m.src == CLIENT).unwrap();
    assert_eq!(
        (m.dst.as_str(), m.confidence.as_deref()),
        (SERVER, Some("probable"))
    );
    let m = p
        .route_matches
        .iter()
        .find(|m| m.src == CLIENT_LIST)
        .unwrap();
    assert_eq!(
        (m.dst.as_str(), m.confidence.as_deref()),
        (SERVER_LIST, Some("verified"))
    );
    // server route -> handler
    assert!(has_edge(
        &p,
        RelType::HandledBy,
        Target::Fact,
        SERVER,
        HANDLER
    ));

    let client = fact(&p, CLIENT);
    assert_eq!(
        (
            client.repo.as_str(),
            client.repo_file.as_deref(),
            client.line
        ),
        ("web", Some("client.go"), Some(10))
    );
    assert_eq!(client.promoted["role"], "client");
    assert_eq!(client.promoted["method"], "GET");
    let server = fact(&p, SERVER);
    assert_eq!(
        (
            server.repo.as_str(),
            server.repo_file.as_deref(),
            server.line
        ),
        ("api", Some("server.go"), Some(27))
    );
    assert_eq!(
        server.name, client.name,
        "same-named client and server routes stay distinct facts"
    );
    assert_eq!(server.promoted["matched_by_clients"], true);
    let handler = fact(&p, HANDLER);
    assert_eq!(
        (
            handler.name.as_str(),
            handler.repo_file.as_deref(),
            handler.line
        ),
        ("example.com/api.getOrder", Some("server.go"), Some(31))
    );
    assert_eq!(handler.promoted["symbol_kind"], "function");
}

#[test]
fn cross_repo_unmatched_call_is_a_route_prop() {
    let p = golden("cross-repo-v0.5.3");
    let u = fact(&p, UNMATCHED);
    assert_eq!(u.name, "/api/v2/{}/orders");
    assert_eq!(u.promoted["unmatched_by_server"], true);
    assert_eq!(u.promoted["unmatched_reason"], "path_unknown");
    assert_eq!(p.stats.unmatched_http_calls, 1);
    assert!(!p.route_matches.iter().any(|m| m.src == UNMATCHED));
}

#[test]
fn cross_repo_counts() {
    let p = golden("cross-repo-v0.5.3");
    let s = &p.stats;
    assert_eq!(
        (
            s.raw_records,
            s.distinct_facts,
            s.multi_record_facts,
            s.conflicting_facts
        ),
        (20, 20, 0, 0)
    );
    assert_eq!((s.resolved_relations, s.ref_relations, s.refs), (17, 10, 8));
    assert_eq!(
        (
            s.route_matches,
            s.callers,
            s.matches_without_id,
            s.skipped_prop_refs
        ),
        (2, 0, 0, 0)
    );
    assert_eq!(s.repos, ["api", "web"]);
    let by = |t: &str| s.relations_by_type.get(t).copied().unwrap_or_default();
    assert_eq!(
        by("ENOLA_DECLARES"),
        TypeCount {
            resolved: 11,
            unresolved: 0
        }
    );
    assert_eq!(
        by("ENOLA_CALLS"),
        TypeCount {
            resolved: 3,
            unresolved: 6
        }
    );
    assert_eq!(
        by("ENOLA_IMPORTS"),
        TypeCount {
            resolved: 0,
            unresolved: 3
        }
    );
    assert_eq!(
        by("ENOLA_HANDLED_BY"),
        TypeCount {
            resolved: 2,
            unresolved: 0
        }
    );
    assert_eq!(
        by("ENOLA_DEPENDS_ON"),
        TypeCount {
            resolved: 1,
            unresolved: 0
        }
    );
    assert_eq!(
        by("ENOLA_INSTANTIATES"),
        TypeCount {
            resolved: 0,
            unresolved: 1
        }
    );
    assert_eq!(s.facts_by_kind["route"], 5);
}

#[test]
fn shared_refs_collect_their_callers() {
    let p = golden("cross-repo-v0.5.3");
    assert!(p.refs.iter().any(|r| r.target == "net/http.Get"));
    let callers: Vec<&str> = group(&p, RelType::Calls, Target::Ref)
        .iter()
        .filter(|e| e.dst == "net/http.Get")
        .map(|e| e.src.as_str())
        .collect();
    assert_eq!(callers, [FETCH_ORDER, FETCH_DYNAMIC]);
    // A target text used by two relation kinds is still one ref node.
    assert_eq!(p.refs.iter().filter(|r| r.target == "net/http").count(), 1);
}

#[test]
fn prefixed_paths_and_modules() {
    let p = golden("cross-repo-v0.5.3");
    let module = p
        .facts
        .iter()
        .find(|f| f.kind == "module" && f.repo == "api")
        .unwrap();
    assert_eq!(
        (module.file.as_deref(), module.repo_file.as_deref()),
        (Some("api/."), Some("."))
    );
    let service = p
        .facts
        .iter()
        .find(|f| f.kind == "service" && f.repo == "web")
        .unwrap();
    assert_eq!(
        (service.file.as_deref(), service.repo_file.as_deref()),
        (None, None)
    );
    assert!(service
        .props_json
        .as_deref()
        .unwrap()
        .contains("edge_coverage"));
}

#[test]
fn single_repo_paths_are_unprefixed() {
    let p = golden("orders-api-v0.5.3");
    assert_eq!(p.stats.repos, ["orders-api"]);
    assert!(p
        .facts
        .iter()
        .filter_map(|f| f.repo_file.as_deref())
        .all(|f| f == "server.go" || f == "go.mod" || f == "."));
    assert!(p.route_matches.is_empty());
}

#[test]
fn projection_is_deterministic() {
    let a = golden("cross-repo-v0.5.3");
    let b = golden("cross-repo-v0.5.3");
    assert_eq!(a, b);
    let rows: Vec<String> = a
        .facts
        .iter()
        .map(|f| serde_json::to_string(f).unwrap())
        .collect();
    let mut sorted = a
        .facts
        .iter()
        .map(|f| f.enola_id.clone())
        .collect::<Vec<_>>();
    sorted.sort();
    assert_eq!(
        a.facts
            .iter()
            .map(|f| f.enola_id.clone())
            .collect::<Vec<_>>(),
        sorted,
        "facts ordered by id"
    );
    assert!(
        rows.iter().all(|r| !r.contains("snapshot_key")),
        "rows carry no store keys"
    );
    // props_json has sorted keys
    let pj = &fact(&a, CLIENT).props_json;
    let keys: Vec<String> = serde_json::from_str::<Map<String, Value>>(pj.as_deref().unwrap())
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let mut sk = keys.clone();
    sk.sort();
    assert_eq!(keys, sk);
}

#[test]
fn duplicate_ids_with_conflicts_keep_raw_records() {
    let a = id(1);
    let recs = records(&[
        json!({"kind":"symbol","name":"p.F","repo":"r","id":a,"file":"f.go","line":3,"props":{"symbol_kind":"function","x":1},"relations":[{"kind":"calls","target":"p.G"}]}),
        json!({"kind":"symbol","name":"p.F","repo":"r","id":a,"file":"f.go","line":9,"props":{"symbol_kind":"function","x":2},"relations":[{"kind":"calls","target":"p.H"}]}),
    ]);
    let p = project(&recs).unwrap();
    let f = &p.facts[0];
    assert_eq!(f.record_count, 2);
    assert_eq!(
        f.line, None,
        "conflicting lines are not chosen by input order"
    );
    assert_eq!(f.conflicts, ["line", "props"]);
    assert_eq!(f.props_json, None);
    assert_eq!(
        f.promoted["symbol_kind"], "function",
        "an agreed key is still promoted"
    );
    assert!(!f.promoted.contains_key("x"));
    let raw: Vec<Value> = serde_json::from_str(f.raw_records.as_deref().unwrap()).unwrap();
    assert_eq!(raw.len(), 2);
    assert_eq!(raw[1]["line"], 9);
    // relations are unioned across records
    assert_eq!(group(&p, RelType::Calls, Target::Ref).len(), 2);
    assert_eq!(
        (p.stats.multi_record_facts, p.stats.conflicting_facts),
        (1, 1)
    );
}

#[test]
fn agreeing_duplicates_are_not_conflicts() {
    let a = id(1);
    let rec = json!({"kind":"symbol","name":"p.F","repo":"r","id":a,"file":"f.go","line":3,"props":{"k":"v"}});
    let p = project(&records(&[rec.clone(), rec])).unwrap();
    let f = &p.facts[0];
    assert!(f.conflicts.is_empty());
    assert_eq!(f.line, Some(3));
    assert_eq!(f.props_json.as_deref(), Some(r#"{"k":"v"}"#));
    assert!(
        f.raw_records.is_some(),
        "raw kept whenever an id has several records"
    );
    assert_eq!(p.stats.conflicting_facts, 0);
}

#[test]
fn unknown_relation_kinds_fall_back_with_their_kind() {
    let (a, b) = (id(1), id(2));
    let p = project(&records(&[
        json!({"kind":"symbol","name":"A","repo":"r","id":a,"relations":[
            {"kind":"teleports","target":"B","target_id":b},
            {"kind":"teleports","target":"Z"},
            {"kind":"summons","target":"B","target_id":b}]}),
        json!({"kind":"symbol","name":"B","repo":"r","id":b}),
    ]))
    .unwrap();
    let rows = group(&p, RelType::Relation, Target::Fact);
    assert_eq!(rows.len(), 2, "two kinds are two edges");
    assert_eq!(
        rows.iter()
            .map(|e| e.kind.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["summons", "teleports"]
    );
    assert_eq!(
        group(&p, RelType::Relation, Target::Ref)[0].kind.as_deref(),
        Some("teleports")
    );
    assert_eq!(RelType::from_kind("calls"), RelType::Calls);
    assert!(RelType::ALL
        .iter()
        .all(|t| t.as_str().starts_with("ENOLA_")));
}

#[test]
fn duplicate_relations_are_deduplicated() {
    let (a, b) = (id(1), id(2));
    let rel = json!({"kind":"calls","target":"B","target_id":b});
    let p = project(&records(&[
        json!({"kind":"symbol","name":"A","repo":"r","id":a,"relations":[rel.clone(), rel]}),
        json!({"kind":"symbol","name":"B","repo":"r","id":b}),
    ]))
    .unwrap();
    assert_eq!(group(&p, RelType::Calls, Target::Fact).len(), 1);
}

#[test]
fn a_dangling_target_id_is_an_artifact_error() {
    let p = project(&records(&[
        json!({"kind":"symbol","name":"A","repo":"r","id":id(1),
        "relations":[{"kind":"calls","target":"B","target_id":id(9)}]}),
    ]));
    match p.unwrap_err() {
        ArtifactError::DanglingTarget {
            target_id, kind, ..
        } => assert_eq!((target_id, kind.as_str()), (id(9), "calls")),
        other => panic!("{other}"),
    }
}

#[test]
fn route_matches_callers_and_their_edge_cases() {
    let (client, server, caller) = (id(1), id(2), id(3));
    let p = project(&records(&[
        json!({"kind":"route","name":"/x","repo":"web","id":client,"props":{"role":"client","caller_id":caller,
            "matched_routes":[{"id":server,"confidence":"verified"},{"name":"/y","confidence":"probable"},{"id":id(9)}]}}),
        json!({"kind":"route","name":"/x","repo":"api","id":client,"props":{"role":"client","caller_id":caller,
            "matched_routes":[{"id":server,"confidence":"probable"}]}}),
        json!({"kind":"route","name":"/x","repo":"api","id":server}),
        json!({"kind":"symbol","name":"web.call","repo":"web","id":caller}),
        json!({"kind":"route","name":"/z","repo":"web","id":id(4),"props":{"caller_id":id(8)}}),
    ]))
    .unwrap();
    let m = &p.route_matches[0];
    assert_eq!(
        (m.src.as_str(), m.dst.as_str()),
        (client.as_str(), server.as_str())
    );
    assert_eq!(m.confidence, None, "records disagree");
    assert_eq!(m.confidences, ["probable", "verified"]);
    assert_eq!(
        p.callers,
        [PairRow {
            src: client,
            dst: caller
        }]
    );
    assert_eq!(p.stats.matches_without_id, 1);
    assert_eq!(
        p.stats.skipped_prop_refs, 2,
        "dangling match id and dangling caller_id"
    );
}

#[test]
fn repo_prefix_is_removed_once_and_only_when_flagged() {
    assert_eq!(
        repo_file(Some("api/api/x.go"), "api", Some(true)).as_deref(),
        Some("api/x.go")
    );
    assert_eq!(
        repo_file(Some("api/x.go"), "api", None).as_deref(),
        Some("api/x.go")
    );
    assert_eq!(
        repo_file(Some("apiary/x.go"), "api", Some(true)).as_deref(),
        Some("apiary/x.go")
    );
    assert_eq!(repo_file(None, "api", Some(true)), None);
}

#[test]
fn an_empty_snapshot_projects_to_nothing() {
    let p = project(&[]).unwrap();
    assert!(p.facts.is_empty() && p.refs.is_empty() && p.edges.is_empty());
    assert_eq!(p.stats.distinct_facts, 0);
}

#[test]
fn nested_props_are_not_promoted() {
    let p = project(&records(&[
        json!({"kind":"route","name":"/x","repo":"r","id":id(1),
        "props":{"method":"GET","type":{"weird":true}}}),
    ]))
    .unwrap();
    assert_eq!(p.facts[0].promoted.get("method"), Some(&json!("GET")));
    assert!(!p.facts[0].promoted.contains_key("type"));
}
