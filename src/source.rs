//! Source identity (PLAN.md D5) and snapshot keys.
//!
//! A *source* is what the user pointed at: a repository directory, a cluster
//! config file, or (with `--snapshot`) a snapshot directory. Its key decides
//! which earlier snapshots a run replaces. Snapshot keys are scoped by source
//! because identical code at two paths yields the same Enola `snapshot_id`.

use crate::hex;
use std::fmt;
use std::path::{Path, PathBuf};

/// Version of the graph projection. Part of every snapshot key, so a future
/// projection change never collides with graphs written by this one.
pub const PROJECTION_VERSION: &str = "v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// A directory indexed as one repository.
    Repository,
    /// An Enola config file (usually a cluster with `repos:`).
    Config,
    /// An existing snapshot directory (`--snapshot`).
    Snapshot,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Repository => "repo",
            Self::Config => "cluster",
            Self::Snapshot => "snapshot",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub kind: SourceKind,
    /// Canonical absolute path (symlinks resolved).
    pub path: PathBuf,
    /// `path:<canonical path>` or `name:<source-name>`.
    pub key: String,
    /// The `--source-name` override, when given.
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputError(pub String);

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InputError {}

/// Resolve the user's `SOURCE` (absent means `cwd`) into a [`Source`].
pub fn resolve(
    source: Option<&str>,
    cwd: &Path,
    snapshot: bool,
    source_name: &str,
) -> Result<Source, InputError> {
    let given = source.unwrap_or(".");
    let raw = cwd.join(given);
    let path = std::fs::canonicalize(&raw)
        .map_err(|e| InputError(format!("SOURCE '{given}' cannot be read: {e}")))?;
    let kind = if snapshot {
        if !path.is_dir() {
            return Err(InputError(format!(
                "--snapshot needs a snapshot directory, and '{given}' is not a directory"
            )));
        }
        if !path.join("receipt.json").is_file() {
            return Err(InputError(format!(
                "'{given}' has no receipt.json, so it is not an Enola snapshot directory \
                 (for a repository, drop --snapshot)"
            )));
        }
        SourceKind::Snapshot
    } else if path.is_dir() {
        SourceKind::Repository
    } else if path.is_file() {
        SourceKind::Config
    } else {
        return Err(InputError(format!(
            "SOURCE '{given}' is neither a directory nor a file"
        )));
    };
    let name = validate_name(source_name)?;
    let key = match &name {
        Some(n) => format!("name:{n}"),
        None => {
            let s = path.to_str().ok_or_else(|| {
                InputError(format!(
                    "the path of '{given}' is not valid UTF-8; pass --source-name to name this source"
                ))
            })?;
            format!("path:{s}")
        }
    };
    Ok(Source {
        kind,
        path,
        key,
        name,
    })
}

fn validate_name(name: &str) -> Result<Option<String>, InputError> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().any(char::is_control) {
        return Err(InputError(
            "--source-name must not contain control characters".into(),
        ));
    }
    if name.len() > 256 {
        return Err(InputError("--source-name must be at most 256 bytes".into()));
    }
    Ok(Some(name.to_string()))
}

/// `v1/<first 16 hex of sha256(source_key)>/<snapshot_id>`.
pub fn snapshot_key(source_key: &str, snapshot_id: &str) -> String {
    let digest = hex::sha256_hex(source_key.as_bytes());
    format!("{PROJECTION_VERSION}/{}/{snapshot_id}", &digest[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn omitted_source_is_the_working_directory() {
        let d = tmp();
        let s = resolve(None, d.path(), false, "").unwrap();
        assert_eq!(s.kind, SourceKind::Repository);
        assert_eq!(s.path, std::fs::canonicalize(d.path()).unwrap());
        assert_eq!(s.key, format!("path:{}", s.path.display()));
        assert_eq!(s.name, None);
    }

    #[test]
    fn spellings_of_one_directory_share_a_key() {
        let d = tmp();
        std::fs::create_dir(d.path().join("repo")).unwrap();
        std::os::unix::fs::symlink(d.path().join("repo"), d.path().join("link")).unwrap();
        let a = resolve(Some("repo"), d.path(), false, "").unwrap();
        let b = resolve(Some("repo/"), d.path(), false, "").unwrap();
        let c = resolve(Some("./link/../repo"), d.path(), false, "").unwrap();
        let e = resolve(Some("link"), d.path(), false, "").unwrap();
        assert_eq!(a.key, b.key);
        assert_eq!(a.key, c.key);
        assert_eq!(a.key, e.key, "symlinks resolve to the same source");
    }

    #[test]
    fn a_file_is_a_config() {
        let d = tmp();
        std::fs::write(d.path().join("cluster.yaml"), "repos: [a, b]\n").unwrap();
        let s = resolve(Some("cluster.yaml"), d.path(), false, "").unwrap();
        assert_eq!(s.kind, SourceKind::Config);
    }

    #[test]
    fn snapshot_mode_requires_a_receipt() {
        let d = tmp();
        let err = resolve(None, d.path(), true, "").unwrap_err();
        assert!(err.0.contains("receipt.json"), "{err}");
        std::fs::write(d.path().join("receipt.json"), "{}").unwrap();
        assert_eq!(
            resolve(None, d.path(), true, "").unwrap().kind,
            SourceKind::Snapshot
        );
        std::fs::write(d.path().join("f.yaml"), "").unwrap();
        assert!(resolve(Some("f.yaml"), d.path(), true, "").is_err());
    }

    #[test]
    fn missing_source_is_an_input_error() {
        let d = tmp();
        let err = resolve(Some("nope"), d.path(), false, "").unwrap_err();
        assert!(err.0.contains("'nope'"), "{err}");
    }

    #[test]
    fn source_name_overrides_the_path() {
        let d = tmp();
        let s = resolve(None, d.path(), false, "  orders-api ").unwrap();
        assert_eq!(s.key, "name:orders-api");
        assert_eq!(s.name.as_deref(), Some("orders-api"));
        assert!(resolve(None, d.path(), false, "bad\nname").is_err());
    }

    #[test]
    fn snapshot_keys_are_scoped_by_source() {
        let id = "sha256:175d56ebd9c60d648ee400894e51c0d2c3beafd3f6c72c5646ee682f834f4eda";
        let a = snapshot_key("path:/a", id);
        let b = snapshot_key("path:/b", id);
        assert_ne!(a, b);
        assert!(a.starts_with("v1/") && a.ends_with(id));
        assert_eq!(a.split('/').nth(1).unwrap().len(), 16);
        assert_eq!(a, snapshot_key("path:/a", id), "deterministic");
    }
}
