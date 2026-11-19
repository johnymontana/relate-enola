//! Writing a projection into the Relate graph (PLAN.md *Run sequence*, steps 4–9).
//!
//! Commit policy is `per-batch`: every `write_cypher` call commits on its own.
//! Writes are idempotent `MERGE`s keyed by snapshot-scoped keys, so a re-run
//! after a crash converges. The `:EnolaSnapshot` node is the completion
//! marker: it is created, and `ENOLA_LATEST` moved to it, in one statement
//! and only after the written graph has been verified. Readers that go through
//! `(:EnolaSource)-[:ENOLA_LATEST]->(:EnolaSnapshot)` never see a partial import.
//!
//! Statement text never contains input data. Relationship type names come
//! from the closed [`RelType`] enum; every value is a parameter.
//!
//! Lookups use single-property unique keys (`fact_key`, `ref_key`) built in
//! Cypher as `$snapshot_key + '/' + id`. A composite `(snapshot_key, enola_id)`
//! key would make the planner seek on `snapshot_key` alone, which every fact
//! of the snapshot shares (Phase 2 finding).

use crate::project::{Projection, RelType, Target};
use crate::snapshot::Snapshot;
use crate::source::{Source, PROJECTION_VERSION};
use crate::wire::{Client, WireError};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

/// Nodes deleted per pruning call.
pub const PRUNE_BATCH: u64 = 5000;

/// Constraints and indexes, created before the first write (idempotent).
pub const SCHEMA: &[&str] = &[
    "CREATE CONSTRAINT enola_source_key IF NOT EXISTS FOR (n:EnolaSource) REQUIRE n.source_key IS UNIQUE",
    "CREATE CONSTRAINT enola_snapshot_key IF NOT EXISTS FOR (n:EnolaSnapshot) REQUIRE n.snapshot_key IS UNIQUE",
    "CREATE CONSTRAINT enola_fact_key IF NOT EXISTS FOR (n:EnolaFact) REQUIRE n.fact_key IS UNIQUE",
    "CREATE CONSTRAINT enola_ref_key IF NOT EXISTS FOR (n:EnolaRef) REQUIRE n.ref_key IS UNIQUE",
    "CREATE INDEX enola_snapshot_source IF NOT EXISTS FOR (n:EnolaSnapshot) ON (n.source_key)",
    "CREATE INDEX enola_fact_snapshot IF NOT EXISTS FOR (n:EnolaFact) ON (n.snapshot_key)",
    "CREATE INDEX enola_fact_source IF NOT EXISTS FOR (n:EnolaFact) ON (n.source_key)",
    "CREATE INDEX enola_ref_snapshot IF NOT EXISTS FOR (n:EnolaRef) ON (n.snapshot_key)",
    "CREATE INDEX enola_ref_source IF NOT EXISTS FOR (n:EnolaRef) ON (n.source_key)",
];

pub const WRITE_FACTS: &str = "UNWIND $rows AS row \
     MERGE (f:EnolaFact {fact_key: $snapshot_key + '/' + row.enola_id}) \
     SET f.snapshot_key = $snapshot_key, f.source_key = $source_key, f.enola_id = row.enola_id, \
         f.kind = row.kind, f.repo = row.repo, f.name = row.name, f.file = row.file, \
         f.repo_file = row.repo_file, f.line = row.line, f.end_line = row.end_line, \
         f.column = row.column, f.end_column = row.end_column, f.record_count = row.record_count, \
         f.props_json = row.props_json, f.raw_records = row.raw_records, f.conflicts = row.conflicts, \
         f += row.promoted";

pub const WRITE_REFS: &str = "UNWIND $rows AS row \
     MERGE (r:EnolaRef {ref_key: $snapshot_key + '/' + row.target}) \
     SET r.snapshot_key = $snapshot_key, r.source_key = $source_key, r.target = row.target";

const MATCH_SRC: &str = "MATCH (a:EnolaFact {fact_key: $snapshot_key + '/' + row.src})";
const MATCH_DST_FACT: &str = "MATCH (b:EnolaFact {fact_key: $snapshot_key + '/' + row.dst})";
const MATCH_DST_REF: &str = "MATCH (b:EnolaRef {ref_key: $snapshot_key + '/' + row.dst})";

/// The statement for one edge group.
pub fn edge_statement(t: RelType, target: Target) -> String {
    let (dst, resolved) = match target {
        Target::Fact => (MATCH_DST_FACT, "true"),
        Target::Ref => (MATCH_DST_REF, "false"),
    };
    let rel = match t {
        RelType::Relation => format!("[r:{} {{kind: row.kind}}]", t.as_str()),
        _ => format!("[r:{}]", t.as_str()),
    };
    format!(
        "UNWIND $rows AS row {MATCH_SRC} {dst} MERGE (a)-{rel}->(b) SET r.resolved = {resolved}"
    )
}

pub const WRITE_ROUTE_MATCHES: &str = "UNWIND $rows AS row \
     MATCH (a:EnolaFact {fact_key: $snapshot_key + '/' + row.src}) \
     MATCH (b:EnolaFact {fact_key: $snapshot_key + '/' + row.dst}) \
     MERGE (a)-[m:ENOLA_MATCHES_ROUTE]->(b) \
     SET m.confidence = row.confidence, m.confidences = row.confidences";

pub const WRITE_CALLERS: &str = "UNWIND $rows AS row \
     MATCH (a:EnolaFact {fact_key: $snapshot_key + '/' + row.src}) \
     MATCH (b:EnolaFact {fact_key: $snapshot_key + '/' + row.dst}) \
     MERGE (a)-[:ENOLA_CALLER]->(b)";

/// The completion marker: one statement, one commit.
pub const MARK: &str = "MERGE (src:EnolaSource {source_key: $source_key}) \
     SET src.source_kind = $source_kind, src.path = $path, src.name = $name \
     MERGE (s:EnolaSnapshot {snapshot_key: $snapshot_key}) \
     ON CREATE SET s += $snapshot, s.imported_at = datetime() \
     WITH src, s \
     OPTIONAL MATCH (src)-[old:ENOLA_LATEST]->(o) WHERE o <> s DELETE old \
     WITH DISTINCT src, s \
     MERGE (src)-[:ENOLA_LATEST]->(s) \
     RETURN s.snapshot_key AS latest";

pub const FIND_SNAPSHOT: &str =
    "MATCH (s:EnolaSnapshot {snapshot_key: $snapshot_key}) RETURN s.facts_digest AS digest";

pub const COUNT_FACTS: &str =
    "MATCH (f:EnolaFact {snapshot_key: $snapshot_key}) RETURN count(f) AS n";
pub const COUNT_REFS: &str =
    "MATCH (r:EnolaRef {snapshot_key: $snapshot_key}) RETURN count(r) AS n";
pub const COUNT_EDGES: &str = "MATCH (a:EnolaFact {snapshot_key: $snapshot_key})-[r]->(b) \
     WHERE type(r) STARTS WITH 'ENOLA_' \
     RETURN type(r) AS t, b:EnolaRef AS to_ref, b.snapshot_key = $snapshot_key AS inside, count(*) AS n";

/// Snapshot keys of this source other than `$snapshot_key`, by label.
pub const OTHER_KEYS: [&str; 3] = [
    "MATCH (n:EnolaFact {source_key: $source_key}) WHERE n.snapshot_key <> $snapshot_key RETURN DISTINCT n.snapshot_key AS k",
    "MATCH (n:EnolaRef {source_key: $source_key}) WHERE n.snapshot_key <> $snapshot_key RETURN DISTINCT n.snapshot_key AS k",
    "MATCH (n:EnolaSnapshot {source_key: $source_key}) WHERE n.snapshot_key <> $snapshot_key RETURN n.snapshot_key AS k",
];
pub const COMPLETED_KEYS: &str =
    "MATCH (n:EnolaSnapshot {source_key: $source_key}) RETURN n.snapshot_key AS k";
pub const DELETE_FACTS: &str = "MATCH (n:EnolaFact {snapshot_key: $old}) WITH n LIMIT $batch DETACH DELETE n RETURN count(*) AS n";
pub const DELETE_REFS: &str = "MATCH (n:EnolaRef {snapshot_key: $old}) WITH n LIMIT $batch DETACH DELETE n RETURN count(*) AS n";
pub const DELETE_SNAPSHOT: &str =
    "MATCH (n:EnolaSnapshot {snapshot_key: $old}) DETACH DELETE n RETURN count(*) AS n";
pub const REMAINING_FACTS: &str = "MATCH (n:EnolaFact {snapshot_key: $old}) RETURN count(n) AS n";
pub const REMAINING_REFS: &str = "MATCH (n:EnolaRef {snapshot_key: $old}) RETURN count(n) AS n";

#[derive(Debug)]
pub enum GraphError {
    Wire(WireError),
    /// The graph read back does not match the rows sent.
    Verify(String),
    /// A completed snapshot with this key exists but its facts differ.
    DigestConflict {
        snapshot_key: String,
        stored: String,
        current: String,
    },
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wire(e) => e.fmt(f),
            Self::Verify(m) => write!(
                f,
                "the written graph does not match the snapshot ({m}); no completion marker was written, \
                 so the import stays invisible — re-run to complete it"
            ),
            Self::DigestConflict { snapshot_key, stored, current } => write!(
                f,
                "snapshot {snapshot_key} is already imported with facts digest {stored}, but this snapshot's \
                 facts digest is {current}; refusing to overwrite"
            ),
        }
    }
}

impl std::error::Error for GraphError {}

impl From<WireError> for GraphError {
    fn from(e: WireError) -> Self {
        Self::Wire(e)
    }
}

/// What a run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub snapshot_key: String,
    /// The snapshot was already imported; nothing was written but the marker.
    pub replay: bool,
    /// Snapshots (or orphaned partial imports) of this source removed.
    pub pruned_snapshots: u64,
    pub pruned_nodes: u64,
    pub write_calls: u64,
    pub elapsed: Duration,
}

/// Everything the writer needs about one import.
pub struct Import<'a> {
    pub source: &'a Source,
    pub snapshot: &'a Snapshot,
    pub projection: &'a Projection,
    pub snapshot_key: &'a str,
    pub keep_snapshots: bool,
}

fn rows<T: Serialize>(items: &[T]) -> impl Iterator<Item = Value> + '_ {
    items
        .iter()
        .map(|r| serde_json::to_value(r).expect("projection rows serialize"))
}

fn scalar(r: &crate::wire::ReadResult, col: &str) -> Option<Value> {
    r.rows.first().and_then(|row| row.get(col)).cloned()
}

fn count(r: &crate::wire::ReadResult) -> u64 {
    scalar(r, "n").and_then(|v| v.as_u64()).unwrap_or(0)
}

impl Import<'_> {
    fn keys(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("snapshot_key".into(), json!(self.snapshot_key));
        m.insert("source_key".into(), json!(self.source.key));
        m
    }

    /// The properties stored on the `:EnolaSnapshot` marker.
    pub fn snapshot_props(&self) -> Value {
        let r = &self.snapshot.receipt;
        let provenance = match (&r.members, &r.git) {
            (Some(m), _) => json!({"members": m}),
            (None, Some(g)) => json!({"repo_path": r.repo_path, "git": g}),
            (None, None) => json!({"repo_path": r.repo_path}),
        };
        let s = &self.projection.stats;
        json!({
            "source_key": self.source.key,
            "snapshot_id": r.snapshot_id,
            "projection_version": PROJECTION_VERSION,
            "enola_version": r.enola_version,
            "extractor_version": r.extractor_version,
            "generated_at": r.generated_at,
            "config_hash": r.config_hash,
            "facts_digest": self.snapshot.facts_digest,
            "raw_count": s.raw_records,
            "distinct_count": s.distinct_facts,
            "repos": s.repos,
            "coverage_json": r.quality.as_ref().map(Value::to_string),
            "provenance_json": provenance.to_string(),
            "stats_json": serde_json::to_string(s).expect("stats serialize"),
        })
    }

    /// Steps 4–9: replay check, schema, writes, verification, marker, prune.
    pub fn run<R: BufRead, W: Write>(
        &self,
        client: &mut Client<R, W>,
    ) -> Result<Outcome, GraphError> {
        let start = Instant::now();
        let calls_before = client.calls();
        let mut outcome = Outcome {
            snapshot_key: self.snapshot_key.to_string(),
            ..Outcome::default()
        };
        let keys = Value::Object(self.keys());

        // 4. Replay check.
        let found = client.read_cypher(FIND_SNAPSHOT, keys.clone())?;
        if let Some(stored) = scalar(&found, "digest") {
            let stored = stored.as_str().unwrap_or_default().to_string();
            if stored != self.snapshot.facts_digest {
                return Err(GraphError::DigestConflict {
                    snapshot_key: self.snapshot_key.to_string(),
                    stored,
                    current: self.snapshot.facts_digest.clone(),
                });
            }
            outcome.replay = true;
        } else {
            // 5. Schema.
            for statement in SCHEMA {
                client.update_schema(statement)?;
            }
            // 6. Writes, in dependency order.
            let extra = self.keys();
            let p = self.projection;
            client.write_rows(WRITE_FACTS, &extra, rows(&p.facts))?;
            client.write_rows(WRITE_REFS, &extra, rows(&p.refs))?;
            for g in &p.edges {
                client.write_rows(&edge_statement(g.rel_type, g.target), &extra, rows(&g.rows))?;
            }
            client.write_rows(WRITE_ROUTE_MATCHES, &extra, rows(&p.route_matches))?;
            client.write_rows(WRITE_CALLERS, &extra, rows(&p.callers))?;
            // 7. Verify.
            self.verify(client)?;
        }

        // 8. Mark (also re-points LATEST on a replay).
        let mut mark = self.keys();
        mark.insert("source_kind".into(), json!(self.source.kind.as_str()));
        mark.insert("path".into(), json!(self.source.path.to_string_lossy()));
        mark.insert("name".into(), json!(self.source.name));
        mark.insert("snapshot".into(), self.snapshot_props());
        client.write_cypher(MARK, Value::Object(mark))?;

        // 9. Prune.
        let (snapshots, nodes) = self.prune(client)?;
        outcome.pruned_snapshots = snapshots;
        outcome.pruned_nodes = nodes;
        outcome.write_calls = client.calls() - calls_before;
        outcome.elapsed = start.elapsed();
        Ok(outcome)
    }

    /// Compare what the store holds for this snapshot with the rows sent.
    fn verify<R: BufRead, W: Write>(&self, client: &mut Client<R, W>) -> Result<(), GraphError> {
        let keys = Value::Object(self.keys());
        let p = self.projection;
        let facts = count(&client.read_cypher(COUNT_FACTS, keys.clone())?);
        if facts != p.facts.len() as u64 {
            return Err(GraphError::Verify(format!(
                "{facts} facts stored, {} expected",
                p.facts.len()
            )));
        }
        let refs = count(&client.read_cypher(COUNT_REFS, keys.clone())?);
        if refs != p.refs.len() as u64 {
            return Err(GraphError::Verify(format!(
                "{refs} refs stored, {} expected",
                p.refs.len()
            )));
        }
        let mut expected: BTreeMap<(String, bool), u64> = BTreeMap::new();
        for g in &p.edges {
            *expected
                .entry((g.rel_type.as_str().into(), g.target == Target::Ref))
                .or_default() += g.rows.len() as u64;
        }
        if !p.route_matches.is_empty() {
            expected.insert(
                ("ENOLA_MATCHES_ROUTE".into(), false),
                p.route_matches.len() as u64,
            );
        }
        if !p.callers.is_empty() {
            expected.insert(("ENOLA_CALLER".into(), false), p.callers.len() as u64);
        }
        let mut actual: BTreeMap<(String, bool), u64> = BTreeMap::new();
        for row in client.read_cypher(COUNT_EDGES, keys)?.rows {
            let t = row["t"].as_str().unwrap_or_default().to_string();
            let n = row["n"].as_u64().unwrap_or(0);
            if row["inside"] != Value::Bool(true) {
                return Err(GraphError::Verify(format!(
                    "{n} {t} relationships leave the snapshot"
                )));
            }
            *actual
                .entry((t, row["to_ref"] == Value::Bool(true)))
                .or_default() += n;
        }
        if actual != expected {
            let diff: Vec<String> = expected
                .keys()
                .chain(actual.keys())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .filter(|k| expected.get(*k) != actual.get(*k))
                .map(|k| {
                    format!(
                        "{}{}: {} stored, {} expected",
                        k.0,
                        if k.1 { " (to refs)" } else { "" },
                        actual.get(k).copied().unwrap_or(0),
                        expected.get(k).copied().unwrap_or(0)
                    )
                })
                .collect();
            return Err(GraphError::Verify(diff.join("; ")));
        }
        Ok(())
    }

    /// Remove this source's other snapshots (all of them, or with
    /// `--keep-snapshots` only orphans that never completed). Returns
    /// (snapshot keys removed, nodes deleted).
    fn prune<R: BufRead, W: Write>(
        &self,
        client: &mut Client<R, W>,
    ) -> Result<(u64, u64), GraphError> {
        let keys = Value::Object(self.keys());
        let mut others: BTreeSet<String> = BTreeSet::new();
        for statement in OTHER_KEYS {
            for row in client.read_cypher(statement, keys.clone())?.rows {
                if let Some(k) = row["k"].as_str() {
                    others.insert(k.to_string());
                }
            }
        }
        if self.keep_snapshots {
            let completed: BTreeSet<String> = client
                .read_cypher(COMPLETED_KEYS, keys)?
                .rows
                .iter()
                .filter_map(|r| r["k"].as_str().map(String::from))
                .collect();
            others.retain(|k| !completed.contains(k));
        }
        let mut nodes = 0;
        for old in &others {
            let params = json!({"old": old, "batch": PRUNE_BATCH});
            for (delete, remaining) in [
                (DELETE_FACTS, REMAINING_FACTS),
                (DELETE_REFS, REMAINING_REFS),
            ] {
                // Progress is read back rather than taken from write counters,
                // so the loop cannot stop early or spin on a host that reports
                // counters differently.
                let mut left = count(&client.read_cypher(remaining, params.clone())?);
                while left > 0 {
                    client.write_cypher(delete, params.clone())?;
                    let now = count(&client.read_cypher(remaining, params.clone())?);
                    if now >= left {
                        return Err(GraphError::Verify(format!(
                            "pruning snapshot {old} made no progress ({now} nodes left)"
                        )));
                    }
                    nodes += left - now;
                    left = now;
                }
            }
            client.write_cypher(DELETE_SNAPSHOT, params)?;
        }
        Ok((others.len() as u64, nodes))
    }
}

/// Literal, parameter-free starter queries for the summary.
pub fn starters() -> Vec<String> {
    vec![
        "MATCH (:EnolaSource)-[:ENOLA_LATEST]->(s:EnolaSnapshot) \
         MATCH (client:EnolaFact {snapshot_key: s.snapshot_key})-[m:ENOLA_MATCHES_ROUTE]->(route:EnolaFact)\
         -[:ENOLA_HANDLED_BY]->(handler:EnolaFact) WHERE client.repo <> route.repo \
         RETURN client.repo AS caller_repo, client.repo_file AS caller_file, client.line AS caller_line, \
         route.method AS method, route.name AS path, handler.repo AS handler_repo, handler.name AS handler, \
         m.confidence AS confidence ORDER BY caller_repo, caller_file, caller_line LIMIT 50"
            .into(),
        "MATCH (:EnolaSource)-[:ENOLA_LATEST]->(s:EnolaSnapshot) \
         MATCH (c:EnolaFact {snapshot_key: s.snapshot_key, kind: 'route', unmatched_by_server: true}) \
         RETURN c.repo, c.repo_file, c.line, c.method, c.name, c.unmatched_reason \
         ORDER BY c.repo, c.repo_file, c.line LIMIT 50"
            .into(),
        "MATCH (:EnolaSource)-[:ENOLA_LATEST]->(s:EnolaSnapshot) \
         MATCH (f:EnolaFact {snapshot_key: s.snapshot_key})-[:ENOLA_CALLS]->(r:EnolaRef) \
         RETURN r.target AS target, count(DISTINCT f) AS callers ORDER BY callers DESC, target LIMIT 25"
            .into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_statements_use_closed_type_names_and_exact_keys() {
        for t in RelType::ALL {
            for target in [Target::Fact, Target::Ref] {
                let s = edge_statement(t, target);
                assert!(s.contains(&format!("[r:{}", t.as_str())), "{s}");
                assert!(s.contains("fact_key: $snapshot_key + '/' + row.src"), "{s}");
                match target {
                    Target::Fact => assert!(
                        s.contains("EnolaFact {fact_key: $snapshot_key + '/' + row.dst}")
                            && s.ends_with("= true")
                    ),
                    Target::Ref => assert!(
                        s.contains("EnolaRef {ref_key: $snapshot_key + '/' + row.dst}")
                            && s.ends_with("= false")
                    ),
                }
                assert_eq!(s.contains("kind: row.kind"), t == RelType::Relation, "{s}");
            }
        }
    }

    #[test]
    fn statements_carry_no_store_keys_inline() {
        // Keys and values are parameters; only label/type names are literal.
        for s in [
            WRITE_FACTS,
            WRITE_REFS,
            WRITE_ROUTE_MATCHES,
            WRITE_CALLERS,
            MARK,
            COUNT_EDGES,
        ] {
            assert!(!s.contains("v1/"), "{s}");
            assert!(s.contains("$snapshot_key"), "{s}");
        }
    }

    #[test]
    fn schema_is_idempotent() {
        assert!(SCHEMA.iter().all(|s| s.contains("IF NOT EXISTS")));
    }
}
