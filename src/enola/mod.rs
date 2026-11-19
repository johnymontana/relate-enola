//! Running the bundled Enola (PLAN.md Phase 3).
//!
//! - [`cache`]: the per-user cache layout (binary, private `HOME`, outputs).
//! - [`bundle`]: the embedded binary and its materialization into the cache.
//! - [`generate`]: what to run for a source, and how to read the result.
//!
//! Enola runs only after the probe (so `relate extension check` never starts
//! it), by absolute path, with an argument vector, a private working
//! directory, null stdin, bounded output capture, and a private `HOME`.

pub mod bundle;
pub mod cache;
pub mod generate;

use std::ffi::OsString;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Bytes of each output stream kept (the tail).
pub const CAPTURE_BYTES: usize = 256 * 1024;

/// One Enola invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub args: Vec<OsString>,
    /// Private working directory (holds no `mcp-arch.yaml`).
    pub cwd: PathBuf,
    /// Environment overrides on top of the inherited environment.
    pub env: Vec<(OsString, OsString)>,
}

/// How an invocation ended.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Exit {
    /// `None` when killed by a signal.
    pub code: Option<i32>,
    pub stdout: String,
    /// Enola writes its log *and* its summary (`Output:`) here.
    pub stderr: String,
}

#[derive(Debug)]
pub enum EnolaError {
    /// This build carries no Enola.
    NotBundled,
    Io(String),
    /// The binary exists but the system cannot execute it (no glibc loader).
    MissingLoader(PathBuf),
    /// Enola exited unsuccessfully; artifacts on disk are not trusted.
    Failed {
        code: Option<i32>,
        tail: String,
    },
    /// Enola succeeded but its artifacts are not where it said.
    NoOutput(String),
}

impl fmt::Display for EnolaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotBundled => f.write_str(
                "this build has no bundled Enola; generate a snapshot with enola and import it with --snapshot",
            ),
            Self::Io(m) => f.write_str(m),
            Self::MissingLoader(p) => write!(
                f,
                "Enola could not start: {} exists but this system lacks the glibc loader it needs \
                 (Alpine: `apk add gcompat`); or generate with enola elsewhere and use --snapshot",
                p.display()
            ),
            Self::Failed { code, tail } => {
                match code {
                    Some(c) => write!(f, "Enola exited with status {c}")?,
                    None => f.write_str("Enola was killed by a signal")?,
                }
                write!(f, "; nothing was imported. Enola said:\n{tail}")
            }
            Self::NoOutput(m) => write!(f, "Enola finished but {m}"),
        }
    }
}

impl std::error::Error for EnolaError {}

/// Runs a [`Plan`]. The process implementation is [`ProcessLauncher`]; tests
/// substitute one that writes fixture artifacts or fails on demand.
pub trait Launcher {
    fn launch(&self, plan: &Plan) -> Result<Exit, EnolaError>;
}

/// Runs the materialized Enola binary.
pub struct ProcessLauncher {
    pub bin: PathBuf,
}

impl Launcher for ProcessLauncher {
    fn launch(&self, plan: &Plan) -> Result<Exit, EnolaError> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(&plan.args)
            .current_dir(&plan.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &plan.env {
            cmd.env(k, v);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && self.bin.exists() => {
                return Err(EnolaError::MissingLoader(self.bin.clone()))
            }
            Err(e) => {
                return Err(EnolaError::Io(format!(
                    "cannot start Enola ({}): {e}",
                    self.bin.display()
                )))
            }
        };
        let out = child
            .stdout
            .take()
            .map(|s| std::thread::spawn(move || tail(s)));
        let err = child
            .stderr
            .take()
            .map(|s| std::thread::spawn(move || tail(s)));
        let status = child
            .wait()
            .map_err(|e| EnolaError::Io(format!("waiting for Enola: {e}")))?;
        let join = |h: Option<std::thread::JoinHandle<String>>| {
            h.and_then(|h| h.join().ok()).unwrap_or_default()
        };
        Ok(Exit {
            code: status.code(),
            stdout: join(out),
            stderr: join(err),
        })
    }
}

/// Read a stream to its end, keeping only the last [`CAPTURE_BYTES`].
fn tail(mut r: impl Read) -> String {
    let mut kept: Vec<u8> = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                kept.extend_from_slice(&buf[..n]);
                if kept.len() > 2 * CAPTURE_BYTES {
                    kept.drain(..kept.len() - CAPTURE_BYTES);
                }
            }
        }
    }
    if kept.len() > CAPTURE_BYTES {
        kept.drain(..kept.len() - CAPTURE_BYTES);
    }
    String::from_utf8_lossy(&kept).into_owned()
}

/// The last `n` lines of `s`.
pub fn last_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn io_err(what: &str, path: &Path, e: std::io::Error) -> EnolaError {
    EnolaError::Io(format!("{what} {}: {e}", path.display()))
}

#[cfg(test)]
mod tests;
