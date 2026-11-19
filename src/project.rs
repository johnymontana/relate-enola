//! Deterministic projection of snapshot records into graph rows (PLAN.md
//! *Graph model*). Pure: no I/O, no host. The rows carry no `snapshot_key` or
//! `source_key`; the writer passes those once per call as parameters.
//!
//! - One [`FactRow`] per distinct fact id. Records sharing an id are combined:
//!   relations are unioned, a field is exposed only when every record agrees,
//!   and disagreements are named in `conflicts` with the original records kept
//!   in `raw_records` (Enola: never let input order choose a value).
//! - Relations with a `target_id` connect two facts; relations without one
//!   point at a shared [`RefRow`] for their target name (D19), never resolved
//!   by name matching.
//! - `matched_routes` and `caller_id` props become route-match and caller edges.

use crate::snapshot::{ArtifactError, Record};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Contract props promoted to top-level node properties when all records agree.
pub const PROMOTED_PROPS: &[&str] = &[
    "symbol_kind",
    "module_role",
    "method",
    "role",
    "type",
    "source",
    "framework",
    "unmatched_by_server",
    "unmatched_reason",
    "matched_by_clients",
    "body_hash",
];

/// Relationship types: Enola's 14 relation kinds plus a fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RelType {
    Declares,
    Imports,
    Calls,
    Implements,
    DependsOn,
    Instantiates,
    Injects,
    HasMethod,
    HandledBy,
    ImplementedBy,
    Publishes,
    Subscribes,
    Exercises,
    Names,
    /// Any kind this projection version does not know; the edge keeps `kind`.
    Relation,
}

impl RelType {
    pub const ALL: [RelType; 15] = [
        Self::Declares,
        Self::Imports,
        Self::Calls,
        Self::Implements,
        Self::DependsOn,
        Self::Instantiates,
        Self::Injects,
        Self::HasMethod,
        Self::HandledBy,
        Self::ImplementedBy,
        Self::Publishes,
        Self::Subscribes,
        Self::Exercises,
        Self::Names,
        Self::Relation,
    ];

    pub fn from_kind(kind: &str) -> Self {
        match kind {
            "declares" => Self::Declares,
            "imports" => Self::Imports,
            "calls" => Self::Calls,
            "implements" => Self::Implements,
            "depends_on" => Self::DependsOn,
            "instantiates" => Self::Instantiates,
            "injects" => Self::Injects,
            "has_method" => Self::HasMethod,
            "handled_by" => Self::HandledBy,
            "implemented_by" => Self::ImplementedBy,
            "publishes" => Self::Publishes,
            "subscribes" => Self::Subscribes,
            "exercises" => Self::Exercises,
            "names" => Self::Names,
            _ => Self::Relation,
        }
    }

    /// The Relate relationship type name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Declares => "ENOLA_DECLARES",
            Self::Imports => "ENOLA_IMPORTS",
            Self::Calls => "ENOLA_CALLS",
            Self::Implements => "ENOLA_IMPLEMENTS",
            Self::DependsOn => "ENOLA_DEPENDS_ON",
            Self::Instantiates => "ENOLA_INSTANTIATES",
            Self::Injects => "ENOLA_INJECTS",
            Self::HasMethod => "ENOLA_HAS_METHOD",
            Self::HandledBy => "ENOLA_HANDLED_BY",
            Self::ImplementedBy => "ENOLA_IMPLEMENTED_BY",
            Self::Publishes => "ENOLA_PUBLISHES",
            Self::Subscribes => "ENOLA_SUBSCRIBES",
            Self::Exercises => "ENOLA_EXERCISES",
            Self::Names => "ENOLA_NAMES",
            Self::Relation => "ENOLA_RELATION",
        }
    }
}

/// What an edge group points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    /// A resolved `target_id`: an `:EnolaFact`.
    Fact,
    /// An unresolved target name: an `:EnolaRef`.
    Ref,
}

/// One `:EnolaFact`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FactRow {
    pub enola_id: String,
    pub kind: String,
    pub repo: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// `file` relative to its repository (one `<repo>/` prefix removed when
    /// the snapshot prefixed it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_column: Option<i64>,
    pub record_count: usize,
    /// All props, serialized with sorted keys, when every record agrees on them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub props_json: Option<String>,
    /// The original JSONL records, when there is more than one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_records: Option<String>,
    /// Names of fields the records disagree on.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<String>,
    /// Promoted contract props (scalar, agreed by every record).
    pub promoted: Map<String, Value>,
}

/// One `:EnolaRef`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct RefRow {
    pub target: String,
}

/// One relationship. `dst` is a fact id (`Target::Fact`) or a ref target
/// (`Target::Ref`). `kind` is set only for [`RelType::Relation`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct EdgeRow {
    pub src: String,
    pub dst: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// All edges of one type to one kind of endpoint: one static Cypher template.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeGroup {
    pub rel_type: RelType,
    pub target: Target,
    pub rows: Vec<EdgeRow>,
}

/// One `ENOLA_MATCHES_ROUTE` (client route → server route).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MatchRow {
    pub src: String,
    pub dst: String,
    /// Set when every record reports the same confidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    /// Every distinct confidence reported, sorted.
    pub confidences: Vec<String>,
}

/// A fact-to-fact pair (`ENOLA_CALLER`: client route → calling symbol).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct PairRow {
    pub src: String,
    pub dst: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TypeCount {
    pub resolved: u64,
    pub unresolved: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Stats {
    pub raw_records: u64,
    pub distinct_facts: u64,
    pub multi_record_facts: u64,
    pub conflicting_facts: u64,
    pub refs: u64,
    pub resolved_relations: u64,
    pub ref_relations: u64,
    pub route_matches: u64,
    pub callers: u64,
    pub unmatched_http_calls: u64,
    /// `matched_routes` entries without an `id` (kept only in `props_json`).
    pub matches_without_id: u64,
    /// `matched_routes[].id` / `caller_id` values naming no fact; skipped.
    pub skipped_prop_refs: u64,
    pub relations_by_type: BTreeMap<String, TypeCount>,
    pub facts_by_kind: BTreeMap<String, u64>,
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Projection {
    pub facts: Vec<FactRow>,
    pub refs: Vec<RefRow>,
    /// Non-empty groups, ordered by type then target.
    pub edges: Vec<EdgeGroup>,
    pub route_matches: Vec<MatchRow>,
    pub callers: Vec<PairRow>,
    pub stats: Stats,
}

/// `file` relative to its repository: exactly one `<repo>/` prefix is removed,
/// and only when the snapshot says it added one.
pub fn repo_file(file: Option<&str>, repo: &str, repo_prefixed: Option<bool>) -> Option<String> {
    let file = file?;
    if repo_prefixed == Some(true) {
        if let Some(rest) = file.strip_prefix(repo).and_then(|r| r.strip_prefix('/')) {
            return Some(rest.to_string());
        }
    }
    Some(file.to_string())
}

/// The value every item agrees on, or `None` (recording `name` in `conflicts`).
fn agreed<T: PartialEq + Clone>(
    values: impl Iterator<Item = T>,
    name: &str,
    conflicts: &mut Vec<String>,
) -> Option<T> {
    let mut it = values;
    let first = it.next()?;
    for v in it {
        if v != first {
            conflicts.push(name.to_string());
            return None;
        }
    }
    Some(first)
}

fn fact_row(id: &str, rs: &[&Record]) -> FactRow {
    let first = &rs[0].fact;
    let mut conflicts = Vec::new();
    // kind/name/repo/file are the id's own inputs, so they agree unless the
    // writer broke its contract; check anyway and report it.
    for (name, differs) in [
        ("kind", rs.iter().any(|r| r.fact.kind != first.kind)),
        ("name", rs.iter().any(|r| r.fact.name != first.name)),
        ("repo", rs.iter().any(|r| r.fact.repo != first.repo)),
        ("file", rs.iter().any(|r| r.fact.file != first.file)),
    ] {
        if differs {
            conflicts.push(name.to_string());
        }
    }
    let line = agreed(rs.iter().map(|r| r.fact.line), "line", &mut conflicts).flatten();
    let end_line = agreed(
        rs.iter().map(|r| r.fact.end_line),
        "end_line",
        &mut conflicts,
    )
    .flatten();
    let column = agreed(rs.iter().map(|r| r.fact.column), "column", &mut conflicts).flatten();
    let end_column = agreed(
        rs.iter().map(|r| r.fact.end_column),
        "end_column",
        &mut conflicts,
    )
    .flatten();
    let props = agreed(rs.iter().map(|r| &r.fact.props), "props", &mut conflicts);

    let mut promoted = Map::new();
    for key in PROMOTED_PROPS {
        let mut values = rs.iter().map(|r| r.fact.props.get(*key));
        let Some(Some(v)) = values.next() else {
            continue;
        };
        let scalar = matches!(v, Value::String(_) | Value::Bool(_) | Value::Number(_));
        if scalar && values.all(|o| o == Some(v)) {
            promoted.insert((*key).to_string(), v.clone());
        }
    }

    // A JSON array of the original lines, byte for byte.
    let raw_records = (rs.len() > 1).then(|| {
        let parts: Vec<&str> = rs.iter().map(|r| r.raw.get()).collect();
        format!("[{}]", parts.join(","))
    });
    FactRow {
        enola_id: id.to_string(),
        kind: first.kind.clone(),
        repo: first.repo.clone(),
        name: first.name.clone(),
        file: first.file.clone(),
        repo_file: repo_file(first.file.as_deref(), &first.repo, first.repo_prefixed),
        line,
        end_line,
        column,
        end_column,
        record_count: rs.len(),
        props_json: props.map(|p| Value::Object(p.clone()).to_string()),
        raw_records,
        conflicts,
        promoted,
    }
}

/// Project validated records into rows.
pub fn project(records: &[Record]) -> Result<Projection, ArtifactError> {
    let mut by_id: BTreeMap<&str, Vec<&Record>> = BTreeMap::new();
    for r in records {
        by_id.entry(r.fact.id.as_str()).or_default().push(r);
    }

    let mut stats = Stats {
        raw_records: records.len() as u64,
        distinct_facts: by_id.len() as u64,
        ..Stats::default()
    };
    let mut repos = BTreeSet::new();
    let mut facts = Vec::with_capacity(by_id.len());
    for (id, rs) in &by_id {
        let row = fact_row(id, rs);
        if rs.len() > 1 {
            stats.multi_record_facts += 1;
        }
        if !row.conflicts.is_empty() {
            stats.conflicting_facts += 1;
        }
        if row.promoted.get("unmatched_by_server") == Some(&Value::Bool(true)) {
            stats.unmatched_http_calls += 1;
        }
        *stats.facts_by_kind.entry(row.kind.clone()).or_default() += 1;
        repos.insert(row.repo.clone());
        facts.push(row);
    }
    stats.repos = repos.into_iter().collect();

    // Relations, deduplicated. The kind is part of the key only for the
    // fallback type, where it distinguishes edges of one Relate type.
    let mut resolved: BTreeSet<(RelType, Option<&str>, &str, &str)> = BTreeSet::new();
    let mut unresolved: BTreeSet<(RelType, Option<&str>, &str, &str)> = BTreeSet::new();
    let mut ref_targets: BTreeSet<&str> = BTreeSet::new();
    let mut matches: BTreeMap<(&str, &str), BTreeSet<&str>> = BTreeMap::new();
    let mut callers: BTreeSet<(&str, &str)> = BTreeSet::new();

    for r in records {
        let src = r.fact.id.as_str();
        for rel in &r.fact.relations {
            let t = RelType::from_kind(&rel.kind);
            let kind = (t == RelType::Relation).then_some(rel.kind.as_str());
            match &rel.target_id {
                Some(tid) => {
                    if !by_id.contains_key(tid.as_str()) {
                        return Err(ArtifactError::DanglingTarget {
                            source_id: src.to_string(),
                            kind: rel.kind.clone(),
                            target_id: tid.clone(),
                        });
                    }
                    resolved.insert((t, kind, src, tid.as_str()));
                }
                None => {
                    ref_targets.insert(rel.target.as_str());
                    unresolved.insert((t, kind, src, rel.target.as_str()));
                }
            }
        }
        if let Some(Value::Array(entries)) = r.fact.props.get("matched_routes") {
            for m in entries {
                match m.get("id").and_then(Value::as_str) {
                    Some(dst) if by_id.contains_key(dst) => {
                        let conf = matches.entry((src, dst)).or_default();
                        if let Some(c) = m.get("confidence").and_then(Value::as_str) {
                            conf.insert(c);
                        }
                    }
                    Some(_) => stats.skipped_prop_refs += 1,
                    None => stats.matches_without_id += 1,
                }
            }
        }
        if let Some(caller) = r.fact.props.get("caller_id").and_then(Value::as_str) {
            if by_id.contains_key(caller) {
                callers.insert((src, caller));
            } else {
                stats.skipped_prop_refs += 1;
            }
        }
    }

    let mut groups: BTreeMap<(RelType, Target), Vec<EdgeRow>> = BTreeMap::new();
    for (set, target) in [(&resolved, Target::Fact), (&unresolved, Target::Ref)] {
        for (t, kind, src, dst) in set {
            let count = stats
                .relations_by_type
                .entry(t.as_str().to_string())
                .or_default();
            match target {
                Target::Fact => count.resolved += 1,
                Target::Ref => count.unresolved += 1,
            }
            groups.entry((*t, target)).or_default().push(EdgeRow {
                src: (*src).to_string(),
                dst: (*dst).to_string(),
                kind: kind.map(String::from),
            });
        }
    }
    stats.resolved_relations = resolved.len() as u64;
    stats.ref_relations = unresolved.len() as u64;
    stats.refs = ref_targets.len() as u64;
    stats.route_matches = matches.len() as u64;
    stats.callers = callers.len() as u64;

    Ok(Projection {
        facts,
        refs: ref_targets
            .into_iter()
            .map(|t| RefRow {
                target: t.to_string(),
            })
            .collect(),
        edges: groups
            .into_iter()
            .map(|((rel_type, target), rows)| EdgeGroup {
                rel_type,
                target,
                rows,
            })
            .collect(),
        route_matches: matches
            .into_iter()
            .map(|((src, dst), confs)| MatchRow {
                src: src.to_string(),
                dst: dst.to_string(),
                confidence: (confs.len() == 1).then(|| confs.iter().next().unwrap().to_string()),
                confidences: confs.into_iter().map(String::from).collect(),
            })
            .collect(),
        callers: callers
            .into_iter()
            .map(|(src, dst)| PairRow {
                src: src.to_string(),
                dst: dst.to_string(),
            })
            .collect(),
        stats,
    })
}

#[cfg(test)]
mod tests;
