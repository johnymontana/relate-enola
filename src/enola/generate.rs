//! What to run for a source, and how to read the result (PLAN.md Phase 3,
//! `notes/enola-contract.md` *Generation*).
//!
//! - **Repository** (a directory): a private config with `repo` = the
//!   absolute path, `output.root` = the cache, history off;
//!   `enola --generate <cfg> --no-cluster`. Nothing is written into the
//!   checkout, and `--no-cluster` keeps a folder of repositories from being
//!   auto-clustered (which would write a `cluster.yaml` into it).
//! - **Config** (a file): `enola --generate <config> --out <cache>/clusters/<hash>`.
//!   The stable `--out` lets Enola answer "Snapshot unchanged" in a fraction
//!   of a second. A single-repository config refuses `--out`; on exactly that
//!   refusal the run is retried without it, and the config's own output
//!   directory is read from Enola's `Output:` line.
//!
//! Artifacts are trusted only after exit status 0. Enola prints its summary,
//! including `Output: <dir>`, on stderr.

use super::cache::Cache;
use super::{io_err, last_lines, EnolaError, Exit, Launcher, Plan};
use crate::hex;
use crate::source::{Source, SourceKind};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Enola's stderr refusal of `--out` for a single-repository config.
pub const OUT_REFUSAL: &str = "--out: names a cluster's directory";
/// Lines of Enola's stderr quoted in a failure.
pub const TAIL_LINES: usize = 15;

/// `YYYY/MM/DD HH:MM:SS ` — the timestamp Go's log package prefixes.
fn strip_timestamp(line: &str) -> &str {
    let b = line.as_bytes();
    let shape = b.len() > 20
        && b[4] == b'/'
        && b[7] == b'/'
        && b[10] == b' '
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b' '
        && [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18]
            .iter()
            .all(|&i| b[i].is_ascii_digit());
    if shape {
        &line[20..]
    } else {
        line
    }
}

/// The part of Enola's stderr worth showing on failure: its routine
/// component-tagged progress lines (`… [engine] …`) are dropped and
/// timestamps removed, keeping errors, warnings and the config line.
pub fn diagnostic(log: &str) -> String {
    let useful: Vec<&str> = log
        .lines()
        .map(strip_timestamp)
        .filter(|l| !l.trim().is_empty() && !l.starts_with('['))
        .collect();
    if useful.is_empty() {
        return last_lines(log, TAIL_LINES);
    }
    useful[useful.len().saturating_sub(TAIL_LINES)..].join("\n")
}

/// A successful generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generated {
    /// The snapshot directory (holds `receipt.json`, `facts.jsonl`).
    pub dir: PathBuf,
    /// Enola found nothing changed and reused the artifacts already there.
    pub unchanged: bool,
}

/// A YAML double-quoted scalar for `s` (a JSON string is one).
fn yaml_string(s: &str) -> String {
    serde_json::to_string(s).expect("strings serialize")
}

fn utf8<'a>(p: &'a Path, what: &str) -> Result<&'a str, EnolaError> {
    p.to_str().ok_or_else(|| {
        EnolaError::Io(format!(
            "the {what} path {} is not valid UTF-8",
            p.display()
        ))
    })
}

/// The private config for directory mode.
pub fn repository_config(repo: &Path, cache: &Cache) -> Result<String, EnolaError> {
    Ok(format!(
        "# Written by enola-to-graph for one run; not a user config.\n\
         repo: {}\n\
         output:\n  dir: \".enola\"\n  root: {}\n\
         history:\n  enabled: false\n",
        yaml_string(utf8(repo, "repository")?),
        yaml_string(utf8(&cache.out_root(), "cache")?),
    ))
}

/// Environment overrides for every Enola run (PLAN.md D20).
pub fn environment(cache: &Cache) -> Vec<(OsString, OsString)> {
    vec![
        ("HOME".into(), cache.home().into_os_string()),
        ("ENOLA_NO_UPDATE_CHECK".into(), "1".into()),
        ("ENOLA_NO_PROMPTS".into(), "1".into()),
    ]
}

/// The `Output:` directory from Enola's summary, if it printed one.
pub fn output_dir(stderr: &str) -> Option<PathBuf> {
    stderr.lines().rev().find_map(|l| {
        let rest = l.trim_start().strip_prefix("Output:")?;
        let p = rest.trim();
        (!p.is_empty()).then(|| PathBuf::from(p))
    })
}

/// True when Enola reported that nothing changed.
pub fn unchanged(stderr: &str) -> bool {
    stderr
        .lines()
        .any(|l| l.trim_start().starts_with("Snapshot unchanged"))
}

/// Generate (or reuse) the snapshot for `source`.
pub fn generate(
    source: &Source,
    cache: &Cache,
    launcher: &dyn Launcher,
) -> Result<Generated, EnolaError> {
    let home = cache.home();
    std::fs::create_dir_all(&home).map_err(|e| io_err("cannot create", &home, e))?;
    let run_dir = cache.new_run_dir()?;
    let result = generate_in(source, cache, launcher, &run_dir);
    let _ = std::fs::remove_dir_all(&run_dir);
    result
}

fn generate_in(
    source: &Source,
    cache: &Cache,
    launcher: &dyn Launcher,
    run_dir: &Path,
) -> Result<Generated, EnolaError> {
    let env = environment(cache);
    let plan = |args: Vec<OsString>| Plan {
        args,
        cwd: run_dir.to_path_buf(),
        env: env.clone(),
    };
    match source.kind {
        SourceKind::Repository => {
            let cfg = run_dir.join("enola-to-graph.yaml");
            std::fs::write(&cfg, repository_config(&source.path, cache)?)
                .map_err(|e| io_err("cannot write", &cfg, e))?;
            let exit = launcher.launch(&plan(vec![
                "--generate".into(),
                cfg.into_os_string(),
                "--no-cluster".into(),
            ]))?;
            interpret(&exit, Some(cache.repo_output(&source.path)))
        }
        SourceKind::Config => {
            let hash = &hex::sha256_hex(source.key.as_bytes())[..16];
            let out = cache.cluster_output(hash);
            let exit = launcher.launch(&plan(vec![
                "--generate".into(),
                source.path.clone().into_os_string(),
                "--out".into(),
                out.clone().into_os_string(),
            ]))?;
            if exit.code == Some(1) && exit.stderr.contains(OUT_REFUSAL) {
                // A single-repository config: it writes to its own output dir.
                let exit = launcher.launch(&plan(vec![
                    "--generate".into(),
                    source.path.clone().into_os_string(),
                ]))?;
                return interpret(&exit, None);
            }
            interpret(&exit, Some(out))
        }
        SourceKind::Snapshot => Err(EnolaError::Io(
            "a snapshot directory is imported, not generated".into(),
        )),
    }
}

/// Read an exit: trust artifacts only after status 0, at Enola's `Output:`
/// directory (else `expected`).
pub fn interpret(exit: &Exit, expected: Option<PathBuf>) -> Result<Generated, EnolaError> {
    if exit.code != Some(0) {
        let log = if exit.stderr.trim().is_empty() {
            &exit.stdout
        } else {
            &exit.stderr
        };
        return Err(EnolaError::Failed {
            code: exit.code,
            tail: diagnostic(log),
        });
    }
    let dir = output_dir(&exit.stderr)
        .or(expected)
        .ok_or_else(|| EnolaError::NoOutput("it did not say where it wrote the snapshot".into()))?;
    if !dir.join("receipt.json").is_file() {
        return Err(EnolaError::NoOutput(format!(
            "{} holds no receipt.json",
            dir.display()
        )));
    }
    Ok(Generated {
        dir,
        unchanged: unchanged(&exit.stderr),
    })
}
