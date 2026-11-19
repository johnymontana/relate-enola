//! The `enola-to-graph` extension child.
//!
//! The Relate host spawns this executable with the request envelope on stdin
//! and drives nothing itself: this child is the MCP client. Phase 1 scope:
//! handshake, the probe that `relate extension check` stops at, and a dry run
//! of `--snapshot` (validate + project, report the counts, write nothing).
//! Phase 2 adds the graph writes; Phase 3 adds bundled generation.

use enola_to_graph::envelope::Envelope;
use enola_to_graph::project::{self, Projection};
use enola_to_graph::snapshot::{self, Snapshot};
use enola_to_graph::source::{self, SourceKind};
use enola_to_graph::wire::{Client, WireError};
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

fn main() {
    match run() {
        Ok(()) => {}
        Err(f) => {
            eprintln!("{NAME}: [{}] {}", f.stage, f.message);
            std::process::exit(1);
        }
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

    // The probe: a cheap read before touching Enola or the user's files.
    // `relate extension check` ends the session here, which is success.
    match client.read_cypher(
        "MATCH (s:EnolaSnapshot) RETURN count(s) AS snapshots",
        serde_json::json!({}),
    ) {
        Ok(_) => {}
        Err(WireError::Closed) => return Ok(()),
        Err(e) => return Err(fail(Stage::Graph, e)),
    }

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

    let snap = snapshot::read_dir(&src.path).map_err(|e| fail(Stage::Artifact, e))?;
    let projection = project::project(&snap.records).map_err(|e| fail(Stage::Artifact, e))?;
    let key = source::snapshot_key(&src.key, &snap.receipt.snapshot_id);
    let text = summary(&snap, &projection, &key);
    client
        .report_output(&text, &[])
        .map_err(|e| fail(Stage::Graph, e))?;
    Ok(())
}

fn summary(snap: &Snapshot, p: &Projection, key: &str) -> String {
    let s = &p.stats;
    format!(
        "dry run (no graph writes in this build): Enola {} snapshot {} — {} records, {} distinct facts \
         ({} with several records), {} resolved relations, {} relations to {} external/unresolved targets, \
         {} route matches, {} callers, {} unmatched HTTP calls; repos: {}; snapshot key {key}",
        snap.receipt.enola_version,
        snap.receipt.snapshot_id,
        s.raw_records,
        s.distinct_facts,
        s.multi_record_facts,
        s.resolved_relations,
        s.ref_relations,
        s.refs,
        s.route_matches,
        s.callers,
        s.unmatched_http_calls,
        s.repos.join(", "),
    )
}
