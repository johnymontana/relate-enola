//! The `enola-to-graph` extension child.
//!
//! The Relate host spawns this executable with the request envelope on stdin;
//! this child is the MCP client and drives the import (PLAN.md *Run
//! sequence*): handshake, the probe that `relate extension check` stops at,
//! source resolution, snapshot validation and projection, then the graph
//! writes in [`enola_to_graph::graph`]. Phase 3 adds bundled generation; until
//! then only `--snapshot` imports.

use enola_to_graph::envelope::Envelope;
use enola_to_graph::graph::{self, GraphError, Import, Outcome};
use enola_to_graph::project::{self, Projection};
use enola_to_graph::snapshot::{self, Snapshot};
use enola_to_graph::source::{self, Source, SourceKind};
use enola_to_graph::wire::{Client, WireError};
use serde_json::Value;
use std::fmt;
use std::io::BufReader;

const NAME: &str = "enola-to-graph";

/// Which part of the run failed (PLAN.md: staged, actionable errors).
#[derive(Debug, Clone, Copy)]
enum Stage {
    Input,
    Enola,
    Artifact,
    Graph,
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Stage::Input => "input",
            Stage::Enola => "enola",
            Stage::Artifact => "artifact",
            Stage::Graph => "graph",
        })
    }
}

struct Failure {
    stage: Stage,
    message: String,
}

fn fail(stage: Stage, e: impl fmt::Display) -> Failure {
    Failure {
        stage,
        message: e.to_string(),
    }
}

fn graph_failure(e: GraphError) -> Failure {
    match e {
        GraphError::DigestConflict { .. } => fail(Stage::Artifact, e),
        other => fail(Stage::Graph, other),
    }
}

fn main() {
    if let Err(f) = run() {
        eprintln!("{NAME}: [{}] {}", f.stage, f.message);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Failure> {
    let mut client = Client::new(
        BufReader::new(std::io::stdin().lock()),
        std::io::stdout().lock(),
    );
    let envelope = client.read_envelope().map_err(|e| fail(Stage::Input, e))?;
    let envelope = Envelope::from_map(&envelope).map_err(|e| fail(Stage::Input, e))?;
    client
        .initialize(NAME, env!("CARGO_PKG_VERSION"))
        .map_err(|e| fail(Stage::Graph, e))?;
    // Test-only fault injection (tests/integration.rs); unset in normal runs.
    client.fail_after(
        std::env::var("ENOLA_TO_GRAPH_TEST_FAIL_AFTER")
            .ok()
            .and_then(|v| v.parse().ok()),
    );

    // 1. The probe: a cheap read before touching Enola or the user's files.
    // `relate extension check` ends the session here, which is success.
    match client.read_cypher(
        "MATCH (s:EnolaSnapshot) RETURN count(s) AS snapshots",
        serde_json::json!({}),
    ) {
        Ok(_) => {}
        Err(WireError::Closed) => return Ok(()),
        Err(e) => return Err(fail(Stage::Graph, e)),
    }

    // 2. Resolve the source.
    let cwd = std::env::current_dir().map_err(|e| fail(Stage::Input, e))?;
    let src = source::resolve(
        envelope.source.as_deref(),
        &cwd,
        envelope.snapshot,
        &envelope.source_name,
    )
    .map_err(|e| fail(Stage::Input, e))?;
    if src.kind != SourceKind::Snapshot {
        return Err(fail(
            Stage::Enola,
            "this build has no bundled Enola; generate a snapshot with enola and import it with --snapshot",
        ));
    }

    // 3. Prepare: validate and project.
    let snap = snapshot::read_dir(&src.path).map_err(|e| fail(Stage::Artifact, e))?;
    let projection = project::project(&snap.records).map_err(|e| fail(Stage::Artifact, e))?;
    let snapshot_key = source::snapshot_key(&src.key, &snap.receipt.snapshot_id);

    // 4–9. Write, verify, mark, prune.
    let outcome = Import {
        source: &src,
        snapshot: &snap,
        projection: &projection,
        snapshot_key: &snapshot_key,
        keep_snapshots: envelope.keep_snapshots,
    }
    .run(&mut client)
    .map_err(graph_failure)?;

    // 10. Summary.
    client
        .report_output(
            &summary(&src, &snap, &projection, &outcome),
            &graph::starters(),
        )
        .map_err(|e| fail(Stage::Graph, e))?;
    Ok(())
}

fn summary(src: &Source, snap: &Snapshot, p: &Projection, o: &Outcome) -> String {
    let r = &snap.receipt;
    let s = &p.stats;
    let mut lines = Vec::new();
    let what = if o.replay {
        "already imported (replay: nothing rewritten; it is the latest for this source)".to_string()
    } else if s.distinct_facts == 0 {
        "imported an EMPTY snapshot (Enola extracted 0 facts; check the extractors it ran)"
            .to_string()
    } else {
        format!(
            "imported {} facts ({} records), {} resolved relations, {} relations to {} external/unresolved targets, \
             {} route matches, {} callers",
            s.distinct_facts,
            s.raw_records,
            s.resolved_relations,
            s.ref_relations,
            s.refs,
            s.route_matches,
            s.callers
        )
    };
    lines.push(format!(
        "{NAME}: Enola {} snapshot {} {what}",
        r.enola_version, r.snapshot_id
    ));
    lines.push(format!(
        "source {} ({}); repos: {}",
        src.key,
        src.kind.as_str(),
        if s.repos.is_empty() {
            "none".into()
        } else {
            s.repos.join(", ")
        }
    ));
    if let Some(q) = &r.quality {
        lines.push(quality_line(q, s.unmatched_http_calls));
    }
    if s.conflicting_facts > 0 {
        lines.push(format!(
            "{} facts have conflicting records (originals kept in raw_records)",
            s.conflicting_facts
        ));
    }
    if o.pruned_snapshots > 0 {
        lines.push(format!(
            "pruned {} older snapshot(s) of this source ({} nodes)",
            o.pruned_snapshots, o.pruned_nodes
        ));
    }
    lines.push(format!(
        "{} host calls in {:.1}s; snapshot key {} — try:",
        o.write_calls,
        o.elapsed.as_secs_f64(),
        o.snapshot_key
    ));
    // The host prints the summary on one line, so segments are sentences.
    lines.join(". ")
}

fn quality_line(q: &Value, unmatched: u64) -> String {
    let n = |k: &str| q.get(k).and_then(Value::as_u64);
    let mut parts = Vec::new();
    if let (Some(parsed), Some(seen)) = (n("files_parsed"), n("files_seen")) {
        parts.push(format!("{parsed}/{seen} files produced facts"));
    }
    if let Some(e) = n("parse_errors") {
        parts.push(format!("{e} parse errors"));
    }
    if let Some(c) = q.get("coverage") {
        if let Some(u) = c.get("unresolved_edges").and_then(Value::as_u64) {
            parts.push(format!("{u} unresolved cross-service edges"));
        }
    }
    parts.push(format!("{unmatched} unmatched HTTP calls"));
    format!("coverage: {}", parts.join(", "))
}
