//! Validate and project a snapshot directory, then print the projection's
//! stats and the serialized row bytes per phase as JSON.
//!
//!     cargo run --release --example project_stats -- path/to/snapshot-dir
//!
//! Used to size real clusters against the host's caps (PLAN.md Phase 1).

use enola_to_graph::project::{self, Target};
use enola_to_graph::snapshot;
use serde::Serialize;
use serde_json::json;
use std::time::Instant;

fn bytes<T: Serialize>(rows: &[T]) -> (usize, u64) {
    let b = rows
        .iter()
        .map(|r| serde_json::to_string(r).unwrap().len() as u64)
        .sum();
    (rows.len(), b)
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: project_stats <snapshot-dir>");
    let t0 = Instant::now();
    let snap = snapshot::read_dir(dir.as_ref()).unwrap_or_else(|e| panic!("{e}"));
    let read_ms = t0.elapsed().as_millis();
    let t1 = Instant::now();
    let p = project::project(&snap.records).unwrap_or_else(|e| panic!("{e}"));
    let project_ms = t1.elapsed().as_millis();

    let (fact_rows, fact_bytes) = bytes(&p.facts);
    let (ref_rows, ref_bytes) = bytes(&p.refs);
    let (mut res_rows, mut res_bytes, mut unres_rows, mut unres_bytes) = (0, 0, 0, 0);
    for g in &p.edges {
        let (n, b) = bytes(&g.rows);
        match g.target {
            Target::Fact => (res_rows, res_bytes) = (res_rows + n, res_bytes + b),
            Target::Ref => (unres_rows, unres_bytes) = (unres_rows + n, unres_bytes + b),
        }
    }
    let (match_rows, match_bytes) = bytes(&p.route_matches);
    let (caller_rows, caller_bytes) = bytes(&p.callers);
    let total = fact_bytes + ref_bytes + res_bytes + unres_bytes + match_bytes + caller_bytes;
    let out = json!({
        "snapshot_id": snap.receipt.snapshot_id,
        "enola_version": snap.receipt.enola_version,
        "read_ms": read_ms,
        "project_ms": project_ms,
        "phases": {
            "facts": {"rows": fact_rows, "bytes": fact_bytes},
            "refs": {"rows": ref_rows, "bytes": ref_bytes},
            "resolved_relations": {"rows": res_rows, "bytes": res_bytes},
            "ref_relations": {"rows": unres_rows, "bytes": unres_bytes},
            "route_matches": {"rows": match_rows, "bytes": match_bytes},
            "callers": {"rows": caller_rows, "bytes": caller_bytes},
        },
        "total_row_bytes": total,
        "edge_groups": p.edges.len(),
        "stats": p.stats,
    });
    eprintln!("{}", serde_json::to_string_pretty(&out).unwrap());
}
