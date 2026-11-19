//! Generation through a fake launcher, and materialization.

use super::bundle::{self, Bundled};
use super::cache::Cache;
use super::generate::{self, generate, interpret, output_dir, OUT_REFUSAL};
use super::*;
use crate::source::{self, SourceKind};
use std::cell::RefCell;
use std::path::Path;

/// A launcher driven by a closure; records every plan it receives.
struct Fake<F: Fn(&Plan, usize) -> Result<Exit, EnolaError>> {
    f: F,
    plans: RefCell<Vec<Plan>>,
}

impl<F: Fn(&Plan, usize) -> Result<Exit, EnolaError>> Launcher for Fake<F> {
    fn launch(&self, plan: &Plan) -> Result<Exit, EnolaError> {
        let n = self.plans.borrow().len();
        self.plans.borrow_mut().push(plan.clone());
        (self.f)(plan, n)
    }
}

fn fake<F: Fn(&Plan, usize) -> Result<Exit, EnolaError>>(f: F) -> Fake<F> {
    Fake {
        f,
        plans: RefCell::new(Vec::new()),
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn write_fixture(name: &str, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for f in ["receipt.json", "facts.jsonl"] {
        std::fs::copy(fixture(name).join(f), to.join(f)).unwrap();
    }
}

fn ok(stderr: String) -> Exit {
    Exit {
        code: Some(0),
        stdout: String::new(),
        stderr,
    }
}

fn summary(kind: &str, out: &Path) -> String {
    format!("2026/11/19 12:00:00 [engine] lots of log\n\nSnapshot {kind}:\n  Facts:       20\n  Output:      {}\n", out.display())
}

struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    cache: Cache,
}

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let cache = Cache::at(root.join("cache"));
    World {
        _tmp: tmp,
        root,
        cache,
    }
}

impl World {
    fn repo(&self, name: &str) -> source::Source {
        let p = self.root.join(name);
        std::fs::create_dir_all(&p).unwrap();
        source::resolve(Some(name), &self.root, false, "").unwrap()
    }
    fn config(&self, name: &str) -> source::Source {
        std::fs::write(self.root.join(name), "repos:\n  - a\n  - b\n").unwrap();
        source::resolve(Some(name), &self.root, false, "").unwrap()
    }
}

#[test]
fn repository_mode_runs_a_private_config_with_no_cluster() {
    let w = world();
    let src = w.repo("svc");
    let expected = w.cache.repo_output(&src.path);
    let seen_cfg = RefCell::new(String::new());
    let l = fake(|plan, _| {
        let cfg = PathBuf::from(&plan.args[1]);
        *seen_cfg.borrow_mut() = std::fs::read_to_string(&cfg).unwrap();
        assert!(
            cfg.starts_with(&plan.cwd),
            "the config lives in the private run dir"
        );
        write_fixture(
            "orders-api-v0.5.3",
            &w.cache.repo_output(&w.root.join("svc")),
        );
        Ok(ok(summary(
            "complete",
            &w.cache.repo_output(&w.root.join("svc")),
        )))
    });
    let g = generate(&src, &w.cache, &l).unwrap();
    assert_eq!(
        g,
        generate::Generated {
            dir: expected.clone(),
            unchanged: false
        }
    );
    let plan = &l.plans.borrow()[0];
    assert_eq!(plan.args[0], "--generate");
    assert_eq!(plan.args[2], "--no-cluster");
    assert!(
        !plan.cwd.exists(),
        "the private run dir is removed afterwards"
    );
    assert!(plan.env.contains(&("HOME".into(), w.cache.home().into())));
    assert!(plan
        .env
        .contains(&("ENOLA_NO_UPDATE_CHECK".into(), "1".into())));
    assert!(plan.env.contains(&("ENOLA_NO_PROMPTS".into(), "1".into())));
    let cfg = seen_cfg.borrow();
    assert!(
        cfg.contains(&format!("repo: \"{}\"", src.path.display())),
        "{cfg}"
    );
    assert!(
        cfg.contains(&format!("root: \"{}\"", w.cache.out_root().display())),
        "{cfg}"
    );
    assert!(cfg.contains("history:\n  enabled: false"), "{cfg}");
    assert!(
        std::fs::read_dir(&src.path).unwrap().next().is_none(),
        "nothing written into the checkout"
    );
    assert!(expected.ends_with(Path::new(src.path.strip_prefix("/").unwrap()).join(".enola")));
}

#[test]
fn config_paths_are_yaml_escaped() {
    let w = world();
    let cfg = generate::repository_config(Path::new("/a/we\"ird\\name: x"), &w.cache).unwrap();
    assert!(cfg.contains(r#"repo: "/a/we\"ird\\name: x""#), "{cfg}");
}

#[test]
fn cluster_mode_uses_a_stable_out_per_source() {
    let w = world();
    let src = w.config("cluster.yaml");
    let l = fake(|plan, _| {
        let out = PathBuf::from(&plan.args[3]);
        write_fixture("cross-repo-v0.5.3", &out);
        Ok(ok(summary("complete", &out)))
    });
    let a = generate(&src, &w.cache, &l).unwrap();
    let b = generate(&src, &w.cache, &l).unwrap();
    assert_eq!(a.dir, b.dir, "same source, same --out");
    let first_args = l.plans.borrow()[0].args[..3].to_vec();
    assert_eq!(
        first_args,
        [
            "--generate".into(),
            src.path.clone().into_os_string(),
            "--out".into()
        ]
    );
    assert!(a.dir.starts_with(w.cache.root().join("clusters")));
    let other = w.config("other.yaml");
    assert_ne!(
        generate(&other, &w.cache, &l).unwrap().dir,
        a.dir,
        "another source, another --out"
    );
    assert_eq!(src.kind, SourceKind::Config);
}

#[test]
fn a_single_repo_config_is_retried_without_out() {
    let w = world();
    let src = w.config("single.yaml");
    let own = w.root.join("their-repo/.enola");
    let l = fake(|plan, n| match n {
        0 => {
            assert!(plan.args.iter().any(|a| a == "--out"));
            Ok(Exit {
                code: Some(1),
                stdout: String::new(),
                stderr: format!("2026/11/19 {OUT_REFUSAL}, and this run indexes one repository"),
            })
        }
        _ => {
            assert!(!plan.args.iter().any(|a| a == "--out"));
            write_fixture("orders-api-v0.5.3", &own);
            Ok(ok(summary("complete", &own)))
        }
    });
    let g = generate(&src, &w.cache, &l).unwrap();
    assert_eq!(g.dir, own, "read from Enola's Output: line");
    assert_eq!(l.plans.borrow().len(), 2);
}

#[test]
fn a_failed_run_is_never_ingested_even_with_stale_artifacts() {
    let w = world();
    let src = w.repo("svc");
    write_fixture("orders-api-v0.5.3", &w.cache.repo_output(&src.path));
    let l = fake(|_, _| {
        Ok(Exit {
            code: Some(1),
            stdout: String::new(),
            stderr: "log\nsnapshot generation failed: walking repo: boom\n".into(),
        })
    });
    match generate(&src, &w.cache, &l).unwrap_err() {
        EnolaError::Failed {
            code: Some(1),
            tail,
        } => assert!(tail.contains("walking repo: boom")),
        other => panic!("{other}"),
    }
    let killed = interpret(
        &Exit {
            code: None,
            ..Exit::default()
        },
        None,
    )
    .unwrap_err();
    assert!(killed.to_string().contains("killed by a signal"));
}

#[test]
fn unchanged_reuses_the_artifacts_in_place() {
    let w = world();
    let src = w.repo("svc");
    let out = w.cache.repo_output(&src.path);
    write_fixture("orders-api-v0.5.3", &out);
    let l = fake(|_, _| Ok(ok(summary("unchanged", &out))));
    let g = generate(&src, &w.cache, &l).unwrap();
    assert!(g.unchanged);
    assert_eq!(g.dir, out);
}

#[test]
fn success_without_artifacts_is_an_error() {
    let w = world();
    let src = w.repo("svc");
    let l = fake(|_, _| Ok(ok(summary("complete", Path::new("/nonexistent/out")))));
    assert!(matches!(
        generate(&src, &w.cache, &l).unwrap_err(),
        EnolaError::NoOutput(_)
    ));
}

#[test]
fn output_line_parsing() {
    assert_eq!(
        output_dir("x\n  Output:      /a b/c\n  Nothing changed"),
        Some(PathBuf::from("/a b/c"))
    );
    assert_eq!(output_dir("no summary"), None);
    assert!(generate::unchanged("\nSnapshot unchanged:\n"));
    assert!(!generate::unchanged("Snapshot complete:"));
    assert_eq!(last_lines("a\nb\nc", 2), "b\nc");
}

#[test]
fn the_real_launcher_captures_status_and_streams() {
    let w = world();
    std::fs::create_dir_all(w.root.join("run")).unwrap();
    let l = ProcessLauncher {
        bin: PathBuf::from("/bin/sh"),
    };
    let plan = Plan {
        args: vec![
            "-c".into(),
            "echo out; echo err >&2; echo \"$HOME\" >&2; test -z \"$(cat)\"; exit 3".into(),
        ],
        cwd: w.root.join("run"),
        env: vec![("HOME".into(), "/private-home".into())],
    };
    let exit = l.launch(&plan).unwrap();
    assert_eq!(exit.code, Some(3));
    assert_eq!(exit.stdout, "out\n");
    assert_eq!(
        exit.stderr, "err\n/private-home\n",
        "env override applied; stdin is empty"
    );
}

#[test]
fn the_real_launcher_bounds_its_capture() {
    let w = world();
    let l = ProcessLauncher {
        bin: PathBuf::from("/bin/sh"),
    };
    let plan = Plan {
        args: vec![
            "-c".into(),
            "head -c 1000000 /dev/zero | tr '\\0' x >&2; echo END >&2".into(),
        ],
        cwd: w.root.clone(),
        env: vec![],
    };
    let exit = l.launch(&plan).unwrap();
    assert_eq!(exit.stderr.len(), CAPTURE_BYTES);
    assert!(exit.stderr.ends_with("END\n"), "the tail is kept");
}

#[test]
fn a_missing_binary_is_an_io_error() {
    let l = ProcessLauncher {
        bin: PathBuf::from("/nonexistent/enola"),
    };
    let plan = Plan {
        args: vec![],
        cwd: PathBuf::from("/"),
        env: vec![],
    };
    assert!(matches!(l.launch(&plan).unwrap_err(), EnolaError::Io(_)));
}

static FAKE_BIN: &[u8] = b"#!/bin/sh\necho fake-enola\n";

fn fake_bundle() -> Bundled {
    let sha: &'static str = Box::leak(crate::hex::sha256_hex(FAKE_BIN).into_boxed_str());
    Bundled {
        bytes: FAKE_BIN,
        sha256: sha,
        version: "0.0.0-test",
    }
}

#[test]
fn materialize_installs_verifies_and_reuses() {
    use std::os::unix::fs::PermissionsExt;
    let w = world();
    let b = fake_bundle();
    let bin = bundle::materialize(&b, &w.cache).unwrap();
    assert_eq!(bin, w.cache.bin_dir(b.sha256).join("enola"));
    assert_eq!(std::fs::read(&bin).unwrap(), FAKE_BIN);
    assert_eq!(
        std::fs::metadata(&bin).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for notice in ["LICENSE", "NOTICE", "NOTICE.enola-to-graph"] {
        assert!(
            bin.with_file_name(notice).is_file(),
            "{notice} beside the binary"
        );
    }
    let out = std::process::Command::new(&bin).output().unwrap();
    assert_eq!(
        out.stdout, b"fake-enola\n",
        "the materialized file executes"
    );

    // A corrupted copy is replaced; a good one is reused.
    std::fs::write(&bin, b"tampered").unwrap();
    bundle::materialize(&b, &w.cache).unwrap();
    assert_eq!(std::fs::read(&bin).unwrap(), FAKE_BIN);
    let before = std::fs::metadata(&bin).unwrap().modified().unwrap();
    bundle::materialize(&b, &w.cache).unwrap();
    assert_eq!(
        std::fs::metadata(&bin).unwrap().modified().unwrap(),
        before,
        "reused, not rewritten"
    );
    let leftovers: Vec<_> = std::fs::read_dir(bin.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "no temporary files left behind");
}

#[test]
fn materialize_refuses_bytes_that_do_not_match() {
    let w = world();
    let b = Bundled {
        bytes: FAKE_BIN,
        sha256: "0000",
        version: "x",
    };
    assert!(bundle::materialize(&b, &w.cache).is_err());
}

#[test]
fn cache_layout() {
    let c = Cache::at("/c");
    assert_eq!(
        c.repo_output(Path::new("/Users/me/repo")),
        PathBuf::from("/c/out/Users/me/repo/.enola")
    );
    assert_eq!(c.cluster_output("abcd"), PathBuf::from("/c/clusters/abcd"));
    assert_eq!(c.home(), PathBuf::from("/c/home"));
    #[cfg(not(feature = "bundled-enola"))]
    assert!(bundle::bundled().is_none());
}

#[test]
fn failure_diagnostics_drop_progress_noise() {
    let log = "enola: using config /x/cluster.yaml\n\
               2026/11/19 14:18:52 [engine] extractor dart: not detected\n\
               2026/11/19 14:18:52 [engine] extractor cache: 2 reused\n\
               2026/11/19 14:19:07 snapshot generation failed for /x/missing: walking repo: lstat /x/missing: no such file or directory\n";
    let d = generate::diagnostic(log);
    assert_eq!(
        d,
        "enola: using config /x/cluster.yaml\nsnapshot generation failed for /x/missing: walking repo: lstat /x/missing: no such file or directory"
    );
    // Only progress lines: fall back to the raw tail rather than nothing.
    assert!(generate::diagnostic("2026/11/19 14:18:52 [engine] a\n").contains("[engine] a"));
}
