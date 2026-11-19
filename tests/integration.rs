//! Integration tests against a real `relate` binary (PLAN.md Phase 2).
//!
//! Set `RELATE_BIN` to a Relate executable (≥ 1.1.0) to run them; CI does.
//! Without it every test prints a skip notice and passes, so the ordinary
//! offline `cargo test` stays independent of Relate.
//!
//! Each test gets its own temporary config dir (the extension is installed
//! there with `--from`), home, sources and stores. Nothing touches the
//! user's real Relate configuration.

use enola_to_graph::hex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CROSS: &str = "cross-repo-v0.5.3";
const ORDERS: &str = "orders-api-v0.5.3";
const CROSS_ID: &str = "sha256:175d56ebd9c60d648ee400894e51c0d2c3beafd3f6c72c5646ee682f834f4eda";
const ORDERS_ID: &str = "sha256:9c303fee5dc65c47bdda3403009d9d6979c1c945b7718ed78ecb0d64a52dae8b";

struct Harness {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    relate: PathBuf,
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn harness() -> Option<Harness> {
    let relate = PathBuf::from(std::env::var_os("RELATE_BIN")?);
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let pkg = root.join("pkg");
    for d in ["pkg", "cfg", "home", "db", "src"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
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
    let h = Harness {
        _tmp: tmp,
        root,
        relate,
    };
    let out = h
        .cmd()
        .args(["extension", "install", "enola-to-graph", "--from"])
        .arg(&pkg)
        .output()
        .unwrap();
    assert!(out.status.success(), "install failed: {}", text(&out));
    Some(h)
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

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

impl Harness {
    fn cmd(&self) -> Command {
        let mut c = Command::new(&self.relate);
        c.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("cfg"))
            .current_dir(&self.root);
        c
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
        .arg(h.root.join("pkg"))
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
