//! Integration tests against a real `relate` binary (PLAN.md Phases 2–3).
//!
//! Set `RELATE_BIN` to a Relate executable (≥ 1.1.0) to run them; CI does.
//! Without it every test prints a skip notice and passes, so the ordinary
//! offline `cargo test` stays independent of Relate. Live-generation tests
//! (module `live`) also need `--features bundled-enola` (with `ENOLA_BIN`).
//!
//! The extension is installed **once** per test binary, into a shared
//! config dir and `HOME` under `CARGO_TARGET_TMPDIR`. Installing a fresh
//! 60 MB executable per test made macOS scan dozens of new binaries at once,
//! which delayed child start-up past the host's 10 s handshake deadline
//! (Phase 3 finding). Each test still gets its own sources and stores.
//! Nothing touches the user's real Relate configuration or home.

use enola_to_graph::hex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

const CROSS: &str = "cross-repo-v0.5.3";
const ORDERS: &str = "orders-api-v0.5.3";
const CROSS_ID: &str = "sha256:175d56ebd9c60d648ee400894e51c0d2c3beafd3f6c72c5646ee682f834f4eda";
const ORDERS_ID: &str = "sha256:9c303fee5dc65c47bdda3403009d9d6979c1c945b7718ed78ecb0d64a52dae8b";

/// The installed extension, shared by every test in this binary.
struct Shared {
    relate: PathBuf,
    dir: PathBuf,
}

static SHARED: OnceLock<Option<Shared>> = OnceLock::new();

struct Harness {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    shared: &'static Shared,
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn base_cmd(relate: &Path, shared: &Path) -> Command {
    let mut c = Command::new(relate);
    c.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", shared.join("home"))
        .env("XDG_CONFIG_HOME", shared.join("cfg"))
        .env("DO_NOT_TRACK", "1");
    c
}

fn install_once() -> Option<&'static Shared> {
    SHARED
        .get_or_init(|| {
            let relate = PathBuf::from(std::env::var_os("RELATE_BIN")?);
            let flavor = if cfg!(feature = "bundled-enola") {
                "bundled"
            } else {
                "default"
            };
            let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("integration-{flavor}"));
            let _ = std::fs::remove_dir_all(&dir);
            for d in ["pkg", "cfg", "home"] {
                std::fs::create_dir_all(dir.join(d)).unwrap();
            }
            let dir = std::fs::canonicalize(&dir).unwrap();
            let pkg = dir.join("pkg");
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("package/extension.json"),
                pkg.join("extension.json"),
            )
            .unwrap();
            std::fs::copy(
                env!("CARGO_BIN_EXE_enola-to-graph"),
                pkg.join("enola-to-graph"),
            )
            .unwrap();
            let out = base_cmd(&relate, &dir)
                .args(["extension", "install", "enola-to-graph", "--from"])
                .arg(&pkg)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "install failed: {}", text(&out));
            Some(Shared { relate, dir })
        })
        .as_ref()
}

fn harness() -> Option<Harness> {
    let shared = install_once()?;
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    for d in ["db", "src"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    Some(Harness {
        _tmp: tmp,
        root,
        shared,
    })
}

macro_rules! harness {
    () => {
        match harness() {
            Some(h) => h,
            None => {
                eprintln!("skipped: set RELATE_BIN to a relate binary to run integration tests");
                return;
            }
        }
    };
}

impl Harness {
    fn cmd(&self) -> Command {
        let mut c = base_cmd(&self.shared.relate, &self.shared.dir);
        c.current_dir(&self.root);
        c
    }

    /// The `HOME` every relate (and so every extension child) runs with.
    fn home(&self) -> PathBuf {
        self.shared.dir.join("home")
    }

    fn pkg(&self) -> PathBuf {
        self.shared.dir.join("pkg")
    }

    fn db(&self, name: &str) -> PathBuf {
        self.root.join("db").join(format!("{name}.rl"))
    }

    /// A fresh source directory under the harness root.
    fn src(&self, name: &str) -> PathBuf {
        let p = self.root.join("src").join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// `relate run enola-to-graph <source> --snapshot -d <db> [extra]`.
    fn import(&self, source: &Path, db: &Path, extra: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = self.cmd();
        c.args(["run", "enola-to-graph"])
            .arg(source)
            .arg("--snapshot")
            .arg("-d")
            .arg(db)
            .args(extra);
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn import_ok(&self, source: &Path, db: &Path, extra: &[&str]) -> String {
        let out = self.import(source, db, extra, &[]);
        assert!(out.status.success(), "import failed: {}", text(&out));
        text(&out)
    }

    fn query(&self, db: &Path, q: &str) -> Vec<Value> {
        let out = self
            .cmd()
            .args(["query", q, "--database"])
            .arg(db)
            .args(["--mode", "jsonl", "--max-rows", "0"])
            .output()
            .unwrap();
        assert!(out.status.success(), "query failed: {q}\n{}", text(&out));
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.starts_with('{'))
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// A query returning one row with column `n`.
    fn n(&self, db: &Path, q: &str) -> i64 {
        self.query(db, q)
            .first()
            .and_then(|r| r["n"].as_i64())
            .unwrap_or(0)
    }

    fn latest_id(&self, db: &Path, source: &Path) -> Option<String> {
        let q = format!(
            "MATCH (src:EnolaSource {{source_key: 'path:{}'}})-[:ENOLA_LATEST]->(s) RETURN s.snapshot_id AS id",
            source.display()
        );
        self.query(db, &q)
            .first()
            .and_then(|r| r["id"].as_str().map(String::from))
    }
}

fn copy_fixture(name: &str, to: &Path) {
    for f in ["receipt.json", "facts.jsonl"] {
        std::fs::copy(fixtures().join(name).join(f), to.join(f)).unwrap();
    }
}

fn fixture_lines(name: &str) -> Vec<String> {
    std::fs::read_to_string(fixtures().join(name).join("facts.jsonl"))
        .unwrap()
        .lines()
        .map(String::from)
        .collect()
}

/// Write a valid snapshot directory from fact lines and a snapshot id.
fn write_snapshot(dir: &Path, lines: &[String], snapshot_id: &str) {
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(dir.join("facts.jsonl"), &body).unwrap();
    let receipt = json!({
        "snapshot_id": snapshot_id,
        "format_version": 1,
        "enola_version": "0.5.3",
        "output_hashes": {"facts.jsonl": hex::sha256_tagged(body.as_bytes())},
        "fact_count": lines.len(),
        "quality": {"files_seen": 1, "files_parsed": 1, "parse_errors": 0},
    });
    std::fs::write(dir.join("receipt.json"), receipt.to_string()).unwrap();
}

fn sid(n: u8) -> String {
    format!("sha256:{n:064x}")
}

const FACTS: &str = "MATCH (f:EnolaFact) RETURN count(f) AS n";
const REFS: &str = "MATCH (r:EnolaRef) RETURN count(r) AS n";
const SNAPSHOTS: &str = "MATCH (s:EnolaSnapshot) RETURN count(s) AS n";
const ENOLA_RELS: &str =
    "MATCH (:EnolaFact)-[r]->() WHERE type(r) STARTS WITH 'ENOLA_' RETURN count(r) AS n";
const BY_TYPE: &str = "MATCH (:EnolaFact)-[r]->(b) WHERE type(r) STARTS WITH 'ENOLA_' \
     RETURN type(r) AS t, b:EnolaRef AS to_ref, count(*) AS n ORDER BY t, to_ref";

#[test]
fn extension_check_passes_without_preparing_anything() {
    let h = harness!();
    let out = h
        .cmd()
        .args(["extension", "check"])
        .arg(h.pkg())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("the extension calls 'read_cypher'"),
        "{}",
        text(&out)
    );
}

#[test]
fn golden_import_answers_the_starter_queries() {
    let h = harness!();
    let (src, db) = (h.src("cross"), h.db("g"));
    copy_fixture(CROSS, &src);
    let out = h.import_ok(&src, &db, &[]);
    assert!(out.contains("imported 20 facts"), "{out}");
    assert!(
        out.contains("MATCH (:EnolaSource)-[:ENOLA_LATEST]"),
        "starters are printed: {out}"
    );

    let calls = h.query(
        &db,
        "MATCH (:EnolaSource)-[:ENOLA_LATEST]->(s:EnolaSnapshot) \
         MATCH (c:EnolaFact {snapshot_key: s.snapshot_key})-[m:ENOLA_MATCHES_ROUTE]->(r:EnolaFact)\
         -[:ENOLA_HANDLED_BY]->(h:EnolaFact) WHERE c.repo <> r.repo \
         RETURN c.repo AS caller, c.repo_file AS file, c.line AS line, r.name AS path, h.name AS handler, \
         m.confidence AS confidence ORDER BY line",
    );
    assert_eq!(
        calls,
        [
            json!({"caller": "web", "file": "client.go", "line": 10, "path": "/api/v2/orders/{id}",
                   "handler": "example.com/api.getOrder", "confidence": "probable"}),
            json!({"caller": "web", "file": "client.go", "line": 11, "path": "/api/v2/orders",
                   "handler": "example.com/api.listOrders", "confidence": "verified"}),
        ]
    );
    let unmatched = h.query(
        &db,
        "MATCH (c:EnolaFact {kind: 'route', unmatched_by_server: true}) RETURN c.name AS name, c.unmatched_reason AS why",
    );
    assert_eq!(
        unmatched,
        [json!({"name": "/api/v2/{}/orders", "why": "path_unknown"})]
    );
    assert_eq!(
        h.n(&db, "MATCH (f:EnolaFact)-[:ENOLA_CALLS]->(:EnolaRef {target: 'net/http.Get'}) RETURN count(f) AS n"),
        2
    );
    assert_eq!(
        (
            h.n(&db, FACTS),
            h.n(&db, REFS),
            h.n(&db, ENOLA_RELS),
            h.n(&db, SNAPSHOTS)
        ),
        (20, 8, 29, 1)
    );
    let marker = h.query(
        &db,
        "MATCH (s:EnolaSnapshot) RETURN s.snapshot_id AS id, s.enola_version AS v, s.raw_count AS raw, \
         s.distinct_count AS distinct, s.facts_digest AS digest",
    );
    assert_eq!(
        marker,
        [
            json!({"id": CROSS_ID, "v": "0.5.3", "raw": 20, "distinct": 20,
                "digest": "sha256:5e230ea16edc9f5e851b78d7e14af463ccd2bd4fc95b8bcba2922bade50a7696"})
        ]
    );
}

#[test]
fn replaying_a_snapshot_is_a_noop() {
    let h = harness!();
    let (src, db) = (h.src("cross"), h.db("r"));
    copy_fixture(CROSS, &src);
    h.import_ok(&src, &db, &[]);
    let before = h.query(&db, BY_TYPE);
    let out = h.import_ok(&src, &db, &[]);
    assert!(out.contains("already imported"), "{out}");
    assert_eq!(h.query(&db, BY_TYPE), before);
    assert_eq!((h.n(&db, FACTS), h.n(&db, SNAPSHOTS)), (20, 1));
}

#[test]
fn a_failed_import_is_invisible_and_a_rerun_converges() {
    let h = harness!();
    let (src, db, fresh) = (h.src("cross"), h.db("resume"), h.db("fresh"));
    copy_fixture(CROSS, &src);
    // probe, replay check, 9 schema statements, facts: the 13th call (refs) fails.
    let out = h.import(&src, &db, &[], &[("ENOLA_TO_GRAPH_TEST_FAIL_AFTER", "12")]);
    assert!(
        !out.status.success(),
        "the injected failure must fail the run: {}",
        text(&out)
    );
    assert!(text(&out).contains("[graph]"), "{}", text(&out));
    assert_eq!(
        h.n(&db, FACTS),
        20,
        "facts committed before the failure stay (per-batch)"
    );
    assert_eq!(h.n(&db, REFS), 0);
    assert_eq!(h.n(&db, SNAPSHOTS), 0, "no completion marker");
    assert_eq!(
        h.latest_id(&db, &src),
        None,
        "nothing is visible through ENOLA_LATEST"
    );

    let out = h.import_ok(&src, &db, &[]);
    assert!(out.contains("imported 20 facts"), "{out}");
    h.import_ok(&src, &fresh, &[]);
    assert_eq!(
        h.query(&db, BY_TYPE),
        h.query(&fresh, BY_TYPE),
        "resumed graph equals a fresh import"
    );
    assert_eq!(
        (h.n(&db, FACTS), h.n(&db, REFS), h.n(&db, SNAPSHOTS)),
        (20, 8, 1)
    );
}

#[test]
fn a_failed_new_snapshot_leaves_the_previous_one_latest() {
    let h = harness!();
    let (src, db) = (h.src("svc"), h.db("keep"));
    copy_fixture(CROSS, &src);
    h.import_ok(&src, &db, &[]);
    copy_fixture(ORDERS, &src);
    let out = h.import(&src, &db, &[], &[("ENOLA_TO_GRAPH_TEST_FAIL_AFTER", "14")]);
    assert!(!out.status.success());
    assert_eq!(h.latest_id(&db, &src).as_deref(), Some(CROSS_ID));
    assert_eq!(
        h.n(&db, "MATCH (:EnolaSource)-[:ENOLA_LATEST]->(s) MATCH (f:EnolaFact {snapshot_key: s.snapshot_key}) RETURN count(f) AS n"),
        20,
        "the previous snapshot is intact"
    );
    let out = h.import_ok(&src, &db, &[]);
    assert!(out.contains("pruned 1 older snapshot"), "{out}");
    assert_eq!(h.latest_id(&db, &src).as_deref(), Some(ORDERS_ID));
    assert_eq!((h.n(&db, FACTS), h.n(&db, SNAPSHOTS)), (10, 1));
}

#[test]
fn new_snapshots_prune_old_ones_unless_kept() {
    let h = harness!();
    let (src, db) = (h.src("svc"), h.db("prune"));
    copy_fixture(CROSS, &src);
    h.import_ok(&src, &db, &[]);
    copy_fixture(ORDERS, &src);
    let out = h.import_ok(&src, &db, &[]);
    assert!(out.contains("pruned 1 older snapshot"), "{out}");
    assert_eq!(
        (h.n(&db, FACTS), h.n(&db, REFS), h.n(&db, SNAPSHOTS)),
        (10, 7, 1)
    );

    // --keep-snapshots retains completed history and moves LATEST.
    copy_fixture(CROSS, &src);
    h.import_ok(&src, &db, &["--keep-snapshots"]);
    assert_eq!((h.n(&db, FACTS), h.n(&db, SNAPSHOTS)), (30, 2));
    assert_eq!(h.latest_id(&db, &src).as_deref(), Some(CROSS_ID));

    // An orphaned partial import is removed even with --keep-snapshots.
    let orders = fixture_lines(ORDERS);
    write_snapshot(&src, &orders, &sid(3));
    assert!(!h
        .import(&src, &db, &[], &[("ENOLA_TO_GRAPH_TEST_FAIL_AFTER", "12")])
        .status
        .success());
    assert_eq!(
        h.n(&db, FACTS),
        40,
        "the orphan's facts are present but unmarked"
    );
    write_snapshot(&src, &orders, &sid(4));
    let out = h.import_ok(&src, &db, &["--keep-snapshots"]);
    assert!(
        out.contains("pruned 1 older snapshot"),
        "the orphan is pruned: {out}"
    );
    assert_eq!((h.n(&db, FACTS), h.n(&db, SNAPSHOTS)), (40, 3));
    assert_eq!(
        h.n(
            &db,
            &format!(
                "MATCH (f:EnolaFact) WHERE f.snapshot_key ENDS WITH '{}' RETURN count(f) AS n",
                sid(3)
            )
        ),
        0
    );
}

#[test]
fn identical_snapshots_from_two_sources_stay_isolated() {
    let h = harness!();
    let (a, b, db) = (h.src("a"), h.src("b"), h.db("iso"));
    copy_fixture(CROSS, &a);
    copy_fixture(CROSS, &b);
    h.import_ok(&a, &db, &[]);
    h.import_ok(&b, &db, &[]);
    assert_eq!(
        (h.n(&db, FACTS), h.n(&db, SNAPSHOTS)),
        (40, 2),
        "same snapshot_id, two snapshot keys"
    );
    assert_eq!(
        h.n(&db, "MATCH (x:EnolaFact)-[r]->(y) WHERE y.snapshot_key IS NOT NULL AND x.snapshot_key <> y.snapshot_key RETURN count(r) AS n"),
        0,
        "no relationship crosses snapshots"
    );
    copy_fixture(ORDERS, &a);
    h.import_ok(&a, &db, &[]);
    assert_eq!(h.latest_id(&db, &a).as_deref(), Some(ORDERS_ID));
    assert_eq!(h.latest_id(&db, &b).as_deref(), Some(CROSS_ID));
    assert_eq!(
        (h.n(&db, FACTS), h.n(&db, SNAPSHOTS)),
        (30, 2),
        "pruning a never touches b"
    );
}

#[test]
fn source_name_overrides_the_path_identity() {
    let h = harness!();
    let (a, b, db) = (h.src("checkout-1"), h.src("checkout-2"), h.db("named"));
    copy_fixture(CROSS, &a);
    copy_fixture(ORDERS, &b);
    h.import_ok(&a, &db, &["--source-name", "orders"]);
    let out = h.import_ok(&b, &db, &["--source-name", "orders"]);
    assert!(out.contains("pruned 1 older snapshot"), "{out}");
    assert_eq!(
        h.n(
            &db,
            "MATCH (s:EnolaSource {source_key: 'name:orders'}) RETURN count(s) AS n"
        ),
        1
    );
    assert_eq!((h.n(&db, FACTS), h.n(&db, SNAPSHOTS)), (10, 1));
}

#[test]
fn unrelated_graph_data_survives_every_operation() {
    let h = harness!();
    let (src, db) = (h.src("svc"), h.db("mixed"));
    h.query(
        &db,
        "CREATE (:Person {name: 'ada'})-[:KNOWS]->(:Person {name: 'bob'}) RETURN 1 AS n",
    );
    copy_fixture(CROSS, &src);
    h.import_ok(&src, &db, &[]);
    h.import_ok(&src, &db, &[]);
    copy_fixture(ORDERS, &src);
    h.import_ok(&src, &db, &[]);
    assert!(!h
        .import(&src, &db, &[], &[("ENOLA_TO_GRAPH_TEST_FAIL_AFTER", "3")])
        .status
        .success());
    assert_eq!(h.n(&db, "MATCH (p:Person) RETURN count(p) AS n"), 2);
    assert_eq!(
        h.n(
            &db,
            "MATCH (:Person)-[k:KNOWS]->(:Person) RETURN count(k) AS n"
        ),
        1
    );
}

#[test]
fn a_corrupt_snapshot_writes_nothing() {
    let h = harness!();
    let (src, db) = (h.src("bad"), h.db("bad"));
    copy_fixture(CROSS, &src);
    let mut facts = std::fs::read(src.join("facts.jsonl")).unwrap();
    facts.extend_from_slice(b"\n");
    std::fs::write(src.join("facts.jsonl"), facts).unwrap();
    let out = h.import(&src, &db, &[], &[]);
    assert!(!out.status.success());
    let t = text(&out);
    assert!(t.contains("[artifact]") && t.contains("retry"), "{t}");
    assert_eq!(h.n(&db, FACTS) + h.n(&db, SNAPSHOTS), 0);
}

#[test]
fn an_empty_snapshot_imports_a_marker() {
    let h = harness!();
    let (src, db) = (h.src("empty"), h.db("empty"));
    write_snapshot(&src, &[], &sid(7));
    let out = h.import_ok(&src, &db, &[]);
    assert!(out.contains("EMPTY snapshot"), "{out}");
    assert_eq!(
        h.n(&db, "MATCH (s:EnolaSnapshot) RETURN s.distinct_count AS n"),
        0
    );
    assert_eq!(h.n(&db, SNAPSHOTS), 1);
    assert_eq!(h.latest_id(&db, &src), Some(sid(7)));
}

#[test]
fn a_different_snapshot_under_an_imported_id_is_refused() {
    let h = harness!();
    let (src, db) = (h.src("svc"), h.db("conflict"));
    write_snapshot(&src, &fixture_lines(ORDERS), &sid(9));
    h.import_ok(&src, &db, &[]);
    write_snapshot(&src, &fixture_lines(CROSS), &sid(9));
    let out = h.import(&src, &db, &[], &[]);
    assert!(!out.status.success());
    let t = text(&out);
    assert!(
        t.contains("[artifact]") && t.contains("refusing to overwrite"),
        "{t}"
    );
    assert_eq!((h.n(&db, FACTS), h.n(&db, SNAPSHOTS)), (10, 1));
}

// --- Generation (Phase 3) -----------------------------------------------------

impl Harness {
    /// `relate run enola-to-graph <source> -d <db> [extra]` (no --snapshot).
    fn generate(&self, source: &Path, db: &Path, extra: &[&str]) -> Output {
        self.cmd()
            .args(["run", "enola-to-graph"])
            .arg(source)
            .arg("-d")
            .arg(db)
            .args(extra)
            .output()
            .unwrap()
    }
}

#[cfg(not(feature = "bundled-enola"))]
#[test]
fn generation_without_a_bundled_enola_is_refused_clearly() {
    let h = harness!();
    let (src, db) = (h.src("repo"), h.db("nogen"));
    let out = h.generate(&src, &db, &[]);
    assert!(!out.status.success());
    let t = text(&out);
    assert!(t.contains("[enola]") && t.contains("--snapshot"), "{t}");
    assert!(!h.home().join(".enola").exists(), "nothing ran Enola");
}

#[cfg(feature = "bundled-enola")]
mod live {
    use super::*;

    fn sources() -> PathBuf {
        fixtures().join("src/cross-repo")
    }

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let dest = to.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copy_dir(&e.path(), &dest);
            } else {
                std::fs::copy(e.path(), dest).unwrap();
            }
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args([
                "-c",
                "user.email=fixture@example.com",
                "-c",
                "user.name=fixture",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", text(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The orders-api fixture as a standalone git repository.
    fn orders_repo(h: &Harness) -> PathBuf {
        let repo = h.src("orders-api");
        copy_dir(&sources().join("api"), &repo);
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "fixture"]);
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/example-org/orders-api.git",
            ],
        );
        repo
    }

    fn snapshot_ids(h: &Harness, db: &Path) -> Vec<String> {
        h.query(
            db,
            "MATCH (:EnolaSource)-[:ENOLA_LATEST]->(s) RETURN s.snapshot_id AS id ORDER BY id",
        )
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
    }

    #[test]
    fn directory_mode_leaves_the_checkout_and_home_untouched() {
        let h = harness!();
        let (repo, db) = (orders_repo(&h), h.db("dir"));
        let out = h.generate(&repo, &db, &[]);
        assert!(out.status.success(), "{}", text(&out));
        assert!(
            text(&out).contains("bundled Enola 0.5.3 generated a snapshot"),
            "{}",
            text(&out)
        );
        assert_eq!(
            snapshot_ids(&h, &db),
            [ORDERS_ID],
            "live generation equals the golden snapshot"
        );
        assert_eq!(
            git(&repo, &["status", "--porcelain", "--ignored"]),
            "",
            "the checkout is untouched"
        );
        assert!(
            !h.home().join(".enola").exists(),
            "the user's ~/.enola is untouched"
        );

        let again = h.generate(&repo, &db, &[]);
        assert!(again.status.success());
        let t = text(&again);
        assert!(
            t.contains("found nothing changed") && t.contains("already imported"),
            "{t}"
        );
    }

    #[test]
    fn cluster_mode_matches_the_golden_snapshot_import() {
        let h = harness!();
        let (cluster, live_db, golden_db) = (h.src("cross"), h.db("live"), h.db("golden"));
        copy_dir(&sources(), &cluster);
        let out = h.generate(&cluster.join("cluster.yaml"), &live_db, &[]);
        assert!(out.status.success(), "{}", text(&out));
        assert_eq!(snapshot_ids(&h, &live_db), [CROSS_ID]);
        let golden = h.src("golden");
        copy_fixture(CROSS, &golden);
        h.import_ok(&golden, &golden_db, &[]);
        assert_eq!(
            h.query(&live_db, BY_TYPE),
            h.query(&golden_db, BY_TYPE),
            "live == --snapshot"
        );
        assert!(!h.home().join(".enola").exists());
    }

    #[test]
    fn an_enola_failure_imports_nothing_despite_stale_artifacts() {
        let h = harness!();
        let (cluster, db) = (h.src("cross"), h.db("fail"));
        copy_dir(&sources(), &cluster);
        let config = cluster.join("cluster.yaml");
        assert!(h.generate(&config, &db, &[]).status.success());
        // Same source, same --out, so the previous artifacts are still on disk.
        std::fs::write(&config, "repos:\n  - api\n  - web\n  - missing-member\n").unwrap();
        let out = h.generate(&config, &db, &[]);
        assert!(!out.status.success());
        let t = text(&out);
        assert!(
            t.contains("[enola]")
                && t.contains("missing-member")
                && t.contains("nothing was imported"),
            "{t}"
        );
        assert!(!t.contains("[engine]"), "progress noise is filtered: {t}");
        assert_eq!(
            snapshot_ids(&h, &db),
            [CROSS_ID],
            "the previous snapshot stays latest"
        );
    }

    #[test]
    fn a_single_repository_config_file_is_generated_without_out() {
        let h = harness!();
        let (repo, db) = (orders_repo(&h), h.db("single"));
        let config = h.root.join("src/single.yaml");
        std::fs::write(
            &config,
            format!(
                "repo: {:?}\nhistory:\n  enabled: false\n",
                repo.to_str().unwrap()
            ),
        )
        .unwrap();
        let out = h.generate(&config, &db, &[]);
        assert!(out.status.success(), "{}", text(&out));
        assert_eq!(snapshot_ids(&h, &db), [ORDERS_ID]);
        // The user's own config writes into its repository, as plain Enola would.
        assert!(repo.join(".enola/receipt.json").is_file());
    }
}
