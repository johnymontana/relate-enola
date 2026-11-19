//! The per-user cache (PLAN.md D9, D20):
//!
//! ```text
//! <cache>/enola/<sha256>/enola    the materialized binary (+ LICENSE, NOTICE)
//! <cache>/home/                   Enola's private HOME (its ~/.enola lives here)
//! <cache>/out/<abs repo path>/.enola/   directory-mode output (output.root)
//! <cache>/clusters/<hash>/        cluster-mode output (--out), one per source
//! <cache>/run/<id>/               private working dir of one run (removed after)
//! ```
//!
//! `<cache>` is `$ENOLA_TO_GRAPH_CACHE_DIR` when set to an absolute path, else
//! `~/Library/Caches/relate-enola` on macOS and `$XDG_CACHE_HOME/relate-enola`
//! (falling back to `~/.cache/relate-enola`) elsewhere. Never `/tmp`, which
//! may be mounted `noexec`.

use super::{io_err, EnolaError};
use std::path::{Path, PathBuf};

pub const CACHE_ENV: &str = "ENOLA_TO_GRAPH_CACHE_DIR";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cache {
    root: PathBuf,
}

fn absolute(v: Option<std::ffi::OsString>) -> Option<PathBuf> {
    v.map(PathBuf::from).filter(|p| p.is_absolute())
}

impl Cache {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The platform cache location (see the module docs).
    pub fn locate() -> Result<Self, EnolaError> {
        if let Some(p) = absolute(std::env::var_os(CACHE_ENV)) {
            return Ok(Self::at(p));
        }
        let home = absolute(std::env::var_os("HOME")).ok_or_else(|| {
            EnolaError::Io(
                "HOME is not set to an absolute path; set ENOLA_TO_GRAPH_CACHE_DIR".into(),
            )
        })?;
        let root = if cfg!(target_os = "macos") {
            home.join("Library/Caches/relate-enola")
        } else {
            absolute(std::env::var_os("XDG_CACHE_HOME"))
                .map(|p| p.join("relate-enola"))
                .unwrap_or_else(|| home.join(".cache/relate-enola"))
        };
        Ok(Self::at(root))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn bin_dir(&self, sha256: &str) -> PathBuf {
        self.root.join("enola").join(sha256)
    }

    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    /// The `output.root` of directory mode.
    pub fn out_root(&self) -> PathBuf {
        self.root.join("out")
    }

    /// Where directory mode's artifacts land for `repo`: Enola mirrors the
    /// repository's absolute path under `output.root`.
    pub fn repo_output(&self, repo: &Path) -> PathBuf {
        let rel = repo.strip_prefix("/").unwrap_or(repo);
        self.out_root().join(rel).join(".enola")
    }

    /// The stable `--out` of cluster mode for one source.
    pub fn cluster_output(&self, source_hash: &str) -> PathBuf {
        self.root.join("clusters").join(source_hash)
    }

    /// A fresh private working directory for one run.
    pub fn new_run_dir(&self) -> Result<PathBuf, EnolaError> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = self
            .root
            .join("run")
            .join(format!("{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| io_err("cannot create", &dir, e))?;
        Ok(dir)
    }
}
