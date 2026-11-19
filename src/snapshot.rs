//! Reading and validating an Enola snapshot directory.
//!
//! The contract (Enola `docs/INTEGRATING.md`, v0.5.3): read `receipt.json`
//! first, reject an unsupported `format_version`, then check every file read
//! against `output_hashes`. The facts are hashed and parsed from the *same*
//! bytes, so a concurrent Enola publish can never mix two generations.

use crate::hex;
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// The artifact format generation this importer understands.
pub const SUPPORTED_FORMAT_VERSION: i64 = 1;
pub const RECEIPT_FILE: &str = "receipt.json";
pub const FACTS_FILE: &str = "facts.jsonl";

/// `receipt.json`. Unknown fields are ignored (new fields are additive).
#[derive(Debug, Clone, Deserialize)]
pub struct Receipt {
    pub snapshot_id: String,
    #[serde(default)]
    pub format_version: i64,
    #[serde(default)]
    pub enola_version: String,
    #[serde(default)]
    pub extractor_version: Option<String>,
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub repo_path: Option<String>,
    /// Single-repo provenance: `{ref, commit, dirty, remote}`.
    #[serde(default)]
    pub git: Option<Value>,
    /// Cluster provenance, one entry per member.
    #[serde(default)]
    pub members: Option<Vec<Value>>,
    #[serde(default)]
    pub config_hash: Option<String>,
    #[serde(default)]
    pub output_hashes: BTreeMap<String, String>,
    #[serde(default)]
    pub fact_count: u64,
    #[serde(default)]
    pub insight_count: Option<u64>,
    #[serde(default)]
    pub quality: Option<Value>,
}

/// One relation of a fact.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Relation {
    pub kind: String,
    pub target: String,
    #[serde(default)]
    pub target_id: Option<String>,
}

/// One `facts.jsonl` record.
#[derive(Debug, Clone, Deserialize)]
pub struct Fact {
    pub kind: String,
    pub name: String,
    pub repo: String,
    pub id: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<i64>,
    #[serde(default)]
    pub end_line: Option<i64>,
    #[serde(default)]
    pub column: Option<i64>,
    #[serde(default)]
    pub end_column: Option<i64>,
    #[serde(default)]
    pub repo_prefixed: Option<bool>,
    #[serde(default)]
    pub props: Map<String, Value>,
    #[serde(default)]
    pub relations: Vec<Relation>,
}

/// A parsed record together with its original JSONL text (exact bytes, kept
/// as text rather than a parsed tree so large snapshots stay cheap).
#[derive(Debug, Clone)]
pub struct Record {
    pub fact: Fact,
    pub raw: Box<RawValue>,
}

/// A validated snapshot.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub dir: PathBuf,
    pub receipt: Receipt,
    /// `sha256:<hex>` of `facts.jsonl`, verified against the receipt.
    pub facts_digest: String,
    /// Records in file order (Enola sorts them before writing).
    pub records: Vec<Record>,
}

#[derive(Debug)]
pub enum ArtifactError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Receipt(String),
    UnsupportedFormat(i64),
    MissingDigest(&'static str),
    DigestMismatch {
        file: &'static str,
        expected: String,
        actual: String,
    },
    /// A `facts.jsonl` line (1-based) that is not a valid fact.
    FactLine {
        line: usize,
        message: String,
    },
    CountMismatch {
        declared: u64,
        actual: u64,
    },
    /// A relation's `target_id` names no fact in the snapshot.
    DanglingTarget {
        source_id: String,
        kind: String,
        target_id: String,
    },
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            Self::Receipt(m) => write!(f, "{RECEIPT_FILE} is invalid: {m}"),
            Self::UnsupportedFormat(v) => write!(
                f,
                "{RECEIPT_FILE} has format_version {v}; this enola-to-graph reads format {SUPPORTED_FORMAT_VERSION} \
                 (upgrade enola-to-graph for newer Enola snapshots)"
            ),
            Self::MissingDigest(file) => write!(
                f,
                "{RECEIPT_FILE} declares no output_hashes entry for {file}, so its bytes cannot be verified"
            ),
            Self::DigestMismatch { file, expected, actual } => write!(
                f,
                "{file} does not match {RECEIPT_FILE} (declared {expected}, read {actual}); the directory is \
                 invalid or Enola is still publishing it — retry after generation completes"
            ),
            Self::FactLine { line, message } => write!(f, "{FACTS_FILE} line {line}: {message}"),
            Self::CountMismatch { declared, actual } => write!(
                f,
                "{FACTS_FILE} holds {actual} records but {RECEIPT_FILE} declares fact_count {declared}"
            ),
            Self::DanglingTarget { source_id, kind, target_id } => write!(
                f,
                "fact {source_id} has a '{kind}' relation to target_id {target_id}, which no fact in the snapshot carries"
            ),
        }
    }
}

impl std::error::Error for ArtifactError {}

fn read(path: &Path) -> Result<Vec<u8>, ArtifactError> {
    std::fs::read(path).map_err(|source| ArtifactError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Read and validate the snapshot in `dir`.
pub fn read_dir(dir: &Path) -> Result<Snapshot, ArtifactError> {
    let receipt_bytes = read(&dir.join(RECEIPT_FILE))?;
    let receipt = parse_receipt(&receipt_bytes)?;
    let facts_bytes = read(&dir.join(FACTS_FILE))?;
    let (facts_digest, records) = parse_facts(&receipt, &facts_bytes)?;
    Ok(Snapshot {
        dir: dir.to_path_buf(),
        receipt,
        facts_digest,
        records,
    })
}

/// Parse and check `receipt.json`.
pub fn parse_receipt(bytes: &[u8]) -> Result<Receipt, ArtifactError> {
    let receipt: Receipt =
        serde_json::from_slice(bytes).map_err(|e| ArtifactError::Receipt(e.to_string()))?;
    if receipt.format_version != SUPPORTED_FORMAT_VERSION {
        return Err(ArtifactError::UnsupportedFormat(receipt.format_version));
    }
    if !hex::is_tagged_sha256(&receipt.snapshot_id) {
        return Err(ArtifactError::Receipt(format!(
            "snapshot_id {:?} is not sha256:<64 hex>",
            receipt.snapshot_id
        )));
    }
    Ok(receipt)
}

/// Verify `bytes` against the receipt and parse them into records.
pub fn parse_facts(
    receipt: &Receipt,
    bytes: &[u8],
) -> Result<(String, Vec<Record>), ArtifactError> {
    let expected = receipt
        .output_hashes
        .get(FACTS_FILE)
        .ok_or(ArtifactError::MissingDigest(FACTS_FILE))?;
    let actual = hex::sha256_tagged(bytes);
    if *expected != actual {
        return Err(ArtifactError::DigestMismatch {
            file: FACTS_FILE,
            expected: expected.clone(),
            actual,
        });
    }
    let mut records = Vec::new();
    let mut lines = bytes.split(|b| *b == b'\n').enumerate().peekable();
    while let Some((i, line)) = lines.next() {
        if line.is_empty() {
            if lines.peek().is_none() {
                break; // the newline that ends the last record
            }
            return Err(ArtifactError::FactLine {
                line: i + 1,
                message: "empty line".into(),
            });
        }
        let raw: Box<RawValue> =
            serde_json::from_slice(line).map_err(|e| ArtifactError::FactLine {
                line: i + 1,
                message: format!("not JSON: {e}"),
            })?;
        let fact: Fact = serde_json::from_str(raw.get()).map_err(|e| ArtifactError::FactLine {
            line: i + 1,
            message: format!("not a fact: {e}"),
        })?;
        if !hex::is_lower_hex(&fact.id, 32) {
            return Err(ArtifactError::FactLine {
                line: i + 1,
                message: format!("id {:?} is not 32 lowercase hex characters", fact.id),
            });
        }
        for r in &fact.relations {
            if let Some(t) = &r.target_id {
                if !hex::is_lower_hex(t, 32) {
                    return Err(ArtifactError::FactLine {
                        line: i + 1,
                        message: format!(
                            "relation target_id {t:?} is not 32 lowercase hex characters"
                        ),
                    });
                }
            }
        }
        records.push(Record { fact, raw });
    }
    let actual_count = records.len() as u64;
    if actual_count != receipt.fact_count {
        return Err(ArtifactError::CountMismatch {
            declared: receipt.fact_count,
            actual: actual_count,
        });
    }
    Ok((actual, records))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn receipt_for(facts: &[u8], count: u64) -> Receipt {
        serde_json::from_value(json!({
            "snapshot_id": format!("sha256:{}", "a".repeat(64)),
            "format_version": 1,
            "enola_version": "0.5.3",
            "output_hashes": {"facts.jsonl": hex::sha256_tagged(facts)},
            "fact_count": count,
        }))
        .unwrap()
    }

    const ID_A: &str = "0123456789abcdef0123456789abcdef";

    fn line(id: &str) -> String {
        format!(r#"{{"kind":"symbol","name":"a.F","repo":"r","id":"{id}"}}"#)
    }

    #[test]
    fn reads_the_cross_repo_golden() {
        let s = read_dir(&fixture("cross-repo-v0.5.3")).unwrap();
        assert_eq!(
            s.receipt.snapshot_id,
            "sha256:175d56ebd9c60d648ee400894e51c0d2c3beafd3f6c72c5646ee682f834f4eda"
        );
        assert_eq!(s.receipt.enola_version, "0.5.3");
        assert_eq!(s.records.len(), 20);
        assert_eq!(
            s.facts_digest,
            "sha256:5e230ea16edc9f5e851b78d7e14af463ccd2bd4fc95b8bcba2922bade50a7696"
        );
        assert_eq!(s.receipt.members.as_ref().map(Vec::len), Some(2));
        assert_eq!(
            s.receipt.quality.as_ref().unwrap()["coverage"]["unresolved_edges"],
            1
        );
    }

    #[test]
    fn reads_the_single_repo_golden() {
        let s = read_dir(&fixture("orders-api-v0.5.3")).unwrap();
        assert_eq!(s.records.len(), 10);
        assert!(s
            .records
            .iter()
            .all(|r| r.fact.repo == "orders-api" && r.fact.repo_prefixed.is_none()));
        assert_eq!(
            s.receipt.git.as_ref().unwrap()["remote"],
            "github.com/example-org/orders-api"
        );
    }

    #[test]
    fn unknown_format_is_refused() {
        let bytes = br#"{"snapshot_id":"sha256:00","format_version":2}"#;
        assert!(matches!(
            parse_receipt(bytes),
            Err(ArtifactError::UnsupportedFormat(2))
        ));
        let zero = br#"{"snapshot_id":"sha256:00"}"#;
        assert!(matches!(
            parse_receipt(zero),
            Err(ArtifactError::UnsupportedFormat(0))
        ));
    }

    #[test]
    fn malformed_snapshot_id_is_refused() {
        let bytes = br#"{"snapshot_id":"abc","format_version":1}"#;
        assert!(matches!(
            parse_receipt(bytes),
            Err(ArtifactError::Receipt(_))
        ));
    }

    #[test]
    fn missing_digest_is_refused() {
        let facts = format!("{}\n", line(ID_A));
        let mut r = receipt_for(facts.as_bytes(), 1);
        r.output_hashes.clear();
        assert!(matches!(
            parse_facts(&r, facts.as_bytes()),
            Err(ArtifactError::MissingDigest(FACTS_FILE))
        ));
    }

    #[test]
    fn mismatched_digest_says_retry() {
        let facts = format!("{}\n", line(ID_A));
        let r = receipt_for(b"other bytes", 1);
        let err = parse_facts(&r, facts.as_bytes()).unwrap_err();
        assert!(matches!(err, ArtifactError::DigestMismatch { .. }));
        assert!(err.to_string().contains("retry"), "{err}");
    }

    #[test]
    fn count_mismatch_is_refused() {
        let facts = format!("{}\n{}\n", line(ID_A), line(ID_A));
        let r = receipt_for(facts.as_bytes(), 1);
        assert!(matches!(
            parse_facts(&r, facts.as_bytes()),
            Err(ArtifactError::CountMismatch {
                declared: 1,
                actual: 2
            })
        ));
    }

    #[test]
    fn duplicate_ids_are_records_not_errors() {
        let facts = format!("{}\n{}\n", line(ID_A), line(ID_A));
        let r = receipt_for(facts.as_bytes(), 2);
        assert_eq!(parse_facts(&r, facts.as_bytes()).unwrap().1.len(), 2);
    }

    #[test]
    fn malformed_lines_name_their_line_number() {
        for (bad, needle) in [
            ("{not json", "not JSON"),
            (r#"{"kind":"symbol"}"#, "not a fact"),
            (&line("NOTHEX"), "32 lowercase hex"),
            (
                r#"{"kind":"symbol","name":"a","repo":"r","id":"0123456789abcdef0123456789abcdef","relations":[{"kind":"calls","target":"b","target_id":"x"}]}"#,
                "target_id",
            ),
        ] {
            let facts = format!("{}\n{bad}\n", line(ID_A));
            let r = receipt_for(facts.as_bytes(), 2);
            match parse_facts(&r, facts.as_bytes()).unwrap_err() {
                ArtifactError::FactLine { line: 2, message } => {
                    assert!(message.contains(needle), "{message}")
                }
                other => panic!("{bad}: {other}"),
            }
        }
    }

    #[test]
    fn inner_empty_lines_are_refused_but_a_trailing_newline_is_not() {
        let ok = format!("{}\n", line(ID_A));
        assert!(parse_facts(&receipt_for(ok.as_bytes(), 1), ok.as_bytes()).is_ok());
        let no_newline = line(ID_A);
        assert!(parse_facts(
            &receipt_for(no_newline.as_bytes(), 1),
            no_newline.as_bytes()
        )
        .is_ok());
        let gap = format!("{}\n\n{}\n", line(ID_A), line(ID_A));
        assert!(matches!(
            parse_facts(&receipt_for(gap.as_bytes(), 2), gap.as_bytes()),
            Err(ArtifactError::FactLine { line: 2, .. })
        ));
    }

    #[test]
    fn an_empty_snapshot_is_valid() {
        let r = receipt_for(b"", 0);
        assert!(parse_facts(&r, b"").unwrap().1.is_empty());
    }

    #[test]
    fn additive_fields_are_tolerated() {
        let facts = r#"{"kind":"new_kind","name":"n","repo":"r","id":"0123456789abcdef0123456789abcdef","future":{"x":1}}"#.to_string() + "\n";
        let r = receipt_for(facts.as_bytes(), 1);
        let (_, recs) = parse_facts(&r, facts.as_bytes()).unwrap();
        assert_eq!(
            recs[0].raw.get(),
            facts.trim_end(),
            "the raw record is the original line, unknown fields included"
        );
    }

    #[test]
    fn missing_directory_is_an_io_error() {
        assert!(matches!(
            read_dir(Path::new("/nonexistent/enola")),
            Err(ArtifactError::Io { .. })
        ));
    }
}
