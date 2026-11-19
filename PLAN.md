# enola-to-graph: implementation plan

Status: rev 5 (2026-11-19). **Phase 0 complete (GO)**: findings in
[`notes/enola-contract.md`](notes/enola-contract.md); revisions R1–R9 folded in.
**Phases 1 and 2 complete**: results recorded under each phase. Next: Phase 3.

Repository: `github.com/johnymontana/relate-enola` (this directory). Not part of
`relatedb/relate`. That repo's ADR, docs and release rules do not apply here.

## Goal

Ship `enola-to-graph`, a third-party Relate command extension released from
`johnymontana/relate-enola`. After

```sh
relate extension install enola-to-graph --github johnymontana/relate-enola@v0.1.0
```

one command turns a local repository or an Enola cluster config into a
persistent, queryable Relate graph. Each platform's release tarball embeds a
pinned Enola binary (v0.5.3 to start, `enola.lock.toml`), so runs are offline
after install. `--snapshot` imports existing Enola artifacts through the same
validation and graph-writing code.

Enola's exported artifacts (`receipt.json`, `facts.jsonl`) are the integration
contract. Relate's extension spec (`docs/api/extensions/spec.md`, *The
Transport contract*) is the host contract. `relate-git-to-graph` is a design
reference, not a dependency.

## Decisions

| # | Decision | Choice |
| --- | --- | --- |
| D1 | Commit policy | `per-batch`, idempotent snapshot-scoped `MERGE` writes, completion marker written last. Phase 0: a mid-size two-repo cluster already needs ~84 MB of rows |
| D2 | Snapshot retention | Keep the latest completed snapshot per source; prune older ones after the new marker commits. `--keep-snapshots` retains history |
| D3 | Release cadence | Snapshot mode merges to `main` as an internal milestone. No tag is pushed until the bundled binary is ready |
| D4 | Git enrichment demo | Single-repo demo (one member imported by `git-to-graph`); document the limit |
| D5 | Source identity | Canonicalized absolute path of the repo dir or cluster config; `--source-name NAME` overrides |
| D6 | Raw record retention | `raw_records` JSON only for fact ids with more than one record or conflicting records (0.5% of ids at scale, all conflicting). `props_json` on every fact (R4) |
| D7 | Checkout side effects | Directory mode: private config sets `output.root` to the cache, so nothing is written into the checkout. Cluster mode: Enola writes each member's `.enola/extractor_cache.json` unless the user's config sets `output.root`; the README says so |
| D8 | Relationship types | One static Cypher template per type in a closed set (14 Enola relation kinds + derived types); no dynamic type names |
| D9 | Bundled binary location | Content-addressed per-user cache dir, never `/tmp` |
| D10 | Distribution | Standalone repo `johnymontana/relate-enola`; install via the `--github` pin tier |
| D11 | Host wire client | Our own small MCP client implementing the spec's Transport contract (the spike in `notes/spike/` is the seed) |
| D12 | macOS signing | Ad-hoc only for v0.1. The release job **explicitly** ad-hoc signs both darwin outputs (`codesign --force -s -`; cross-built x86_64 and Enola's own darwin-amd64 are unsigned otherwise), then verifies. Notarization deferred |
| D13 | Extension name | `enola-to-graph`. Assets `enola-to-graph-<version>-<os>-<arch>.tar.gz` |
| D14 | Linux target | `x86_64/aarch64-unknown-linux-musl`. Static, though no extra reach: Relate 1.1.0 itself needs glibc ≥ 2.39, while Enola needs ≥ 2.17 |
| D15 | License | Apache-2.0 for our code; Enola's `LICENSE` and `NOTICE` (Apache-2.0 plus vendored MIT tree-sitter) shipped for the embedded binary |
| D16 | Host compatibility | Relate ≥ 1.1.0; CI integration tests against pinned v1.1.0 and the newest Relate release |
| D17 | Enola upgrades | Manual bump PR: one script updates `enola.lock.toml`, regenerates the goldens, and CI proves the projection still holds |
| D18 | Golden fixture | Committed under `tests/fixtures/` (see `GENERATED.md`). Drift check compares `facts.jsonl` bytes and `snapshot_id` (both deterministic across platforms and paths), never receipt bytes |
| D19 | Unresolved targets (R1) | Shared `:EnolaRef {snapshot_key, target}` nodes, reached by the same typed relationship as resolved targets with `resolved: false` |
| D20 | Enola's `$HOME` | Run Enola with `HOME=<cache>/home` in both modes, so the user's `~/.enola` is never touched |

### Facts this plan relies on

From `relatedb/relate` main @ `0cce8f6d` and the Phase 0 spike against Relate 1.1.0:

- **Envelope:** the positional arrives on stdin under its declared name verbatim (`"SOURCE"`) and is absent when omitted (→ use the working directory). The child never sees `-d`.
- **`extension check`:** ends the session at the child's first `tools/call`. The probe design was verified: check passes without extracting Enola.
- **Deadlines:** the mount enforces a 10-second deadline on the child's first request only, so long Enola runs after `initialize` are safe.
- **Package shape:** a package is exactly two files (`extension.json` and the executable), so Enola is embedded with `include_bytes!` (~59 MB executable).
- **`--github` tier:**
  - The asset must be named `<name>-<version>-<os>-<arch>.tar.gz`, and GitHub must have recorded its `digest`.
  - The install records `{repo, tag, commit, assetSha256}` in the user's trust store and refuses a moved tag.
  - Releases must be immutable.
- **Signing:** relate's installer sets no quarantine, so Gatekeeper doesn't assess installed binaries. The extracted Enola keeps a valid signature (only `com.apple.provenance` is set).
- **Caps:** `per-batch` has no whole-session cap. Per-call caps (8 MiB per statement or line, 1000 rows, 4096 statements) are republished in `initialize`.
- **`git-to-graph` identity:** it keys `(:File {path})` repo-relative with no repo name (D4).

## CLI contract

```sh
relate extension install enola-to-graph --github johnymontana/relate-enola@v0.1.0

relate run enola-to-graph . -d ../architecture.rl                # one repository
relate run enola-to-graph ./cluster.yaml -d ./architecture.rl   # cluster config
relate run enola-to-graph ./snapshot --snapshot -d ./arch.rl    # existing artifacts
relate run enola-to-graph . --source-name api -d ./arch.rl      # stable source identity
```

`package/extension.json`:

```json
{
  "kind": "command",
  "name": "enola-to-graph",
  "version": "0.1.0",
  "protocol": "2025-06-18",
  "flags": [
    {"kind": "boolean", "name": "snapshot", "default": false},
    {"kind": "string",  "name": "source-name", "default": ""},
    {"kind": "boolean", "name": "keep-snapshots", "default": false}
  ],
  "positional": {"name": "SOURCE"},
  "requires-fresh-store": false,
  "commit": "per-batch",
  "executable": "enola-to-graph",
  "hidden": false
}
```

| Input | Behavior |
| --- | --- |
| Directory, or `SOURCE` omitted | One repository. Private config (`repo` = absolute path, `output.root` = `<cache>/out`, `history.enabled: false`), `enola --generate <cfg> --no-cluster` from a private working dir. `--no-cluster` stops Enola auto-clustering a folder of repos and writing a `cluster.yaml` into it. Nothing is written into the checkout. |
| Cluster config file | `enola --generate <abs config> --out <cache>/clusters/<sha(source_key)>`. The stable `--out` enables Enola's 0.3 s "unchanged" no-op. Member paths are config-relative. The config may run its configured providers; members get `.enola/extractor_cache.json` unless the config sets `output.root` (README). |
| `SOURCE --snapshot` | `SOURCE` must be a snapshot directory. Validate and import without starting Enola. |
| `--source-name NAME` | Overrides the canonical-path source identity (D5). |
| `--keep-snapshots` | Skip pruning older snapshots for this source (D2). |
| `-d PATH` | The host owns the store; the child never sees it. |

`<cache>` is `~/Library/Caches/relate-enola` (macOS) or `$XDG_CACHE_HOME/relate-enola`
(falling back to `~/.cache/relate-enola`). Enola runs with `HOME=<cache>/home`,
`ENOLA_NO_UPDATE_CHECK=1` and `ENOLA_NO_PROMPTS=1` (D20).

The final summary reports the Enola version (from the receipt), snapshot ID,
source key, raw record count, distinct facts, resolved edges, ref targets,
unmatched HTTP calls, route matches, receipt coverage (`quality.coverage`,
`parse_errors`, `files_parsed`/`files_seen`), and how many snapshots were pruned.
It says explicitly when the snapshot was already imported (replay) and when it
contains 0 facts. Starters are literal, parameter-free Cypher that resolves the
latest snapshot via `:EnolaSource`.

Errors are actionable and staged: `input`, `enola` (generation), `artifact`
(validation), `graph` (host write), each with a bounded diagnostic (the last
~40 lines of Enola's stderr) and a nonzero exit. Exec `ENOENT` on an existing
cached binary maps to "missing glibc loader (Alpine: `apk add gcompat`); use --snapshot".

## Graph model

Identity:

- `source_key` = `path:<canonical absolute path>` or `name:<source-name>`.
- `snapshot_key` = `v1/<first 16 hex chars of sha256(source_key)>/<snapshot_id>`.
  Scoping by source is required: Phase 0 showed identical sources at different paths produce the same Enola `snapshot_id`.

| Element | Identity | Data |
| --- | --- | --- |
| `:EnolaSource` | unique `source_key` | `source_kind` (repo, cluster, snapshot), display path, `--source-name` if any |
| `:EnolaSnapshot` | unique `snapshot_key` | `source_key`, `snapshot_id`, `projection_version`, `enola_version`, `extractor_version`, facts digest, `raw_count`, `distinct_count`, `coverage_json` (`quality`), `provenance_json` (`git` or `members[]`), `imported_at` (excluded from comparisons). **Its existence is the completion marker.** |
| `(:EnolaSource)-[:ENOLA_LATEST]->(:EnolaSnapshot)` | one per source | Moved in the same statement that creates the marker |
| `:EnolaFact` | unique `fact_key` = `<snapshot_key>/<enola_id>` (also stored: `snapshot_key`, `enola_id`) | `source_key`, `kind`, `repo`, `name`, `repo_file` (`file` with exactly one `<repo>/` prefix removed when `repo_prefixed`; module `.` kept), `line`, `end_line`, `record_count`, `props_json` (always; deterministic). Promoted contract props when all records agree: `symbol_kind`, `module_role`, route `method`, `role`, `type`, `source`, `framework`, `unmatched_by_server`, `unmatched_reason`, `matched_by_clients`, `body_hash`. `raw_records` only when `record_count > 1` or records conflict (D6) |
| `:EnolaRef` (D19) | unique `ref_key` = `<snapshot_key>/<target>` | `source_key`, `target` (the exported name spelling). Never resolved by name matching |
| Relations | deduplicated on `(source id, kind, target_id \| target)` | Static types: `ENOLA_DECLARES`, `ENOLA_IMPORTS`, `ENOLA_CALLS`, `ENOLA_IMPLEMENTS`, `ENOLA_DEPENDS_ON`, `ENOLA_INSTANTIATES`, `ENOLA_INJECTS`, `ENOLA_HAS_METHOD`, `ENOLA_HANDLED_BY`, `ENOLA_IMPLEMENTED_BY`, `ENOLA_PUBLISHES`, `ENOLA_SUBSCRIBES`, `ENOLA_EXERCISES`, `ENOLA_NAMES`; unknown kinds become `ENOLA_RELATION {kind}`. With `target_id`: to the `:EnolaFact` (`resolved: true`). Without: to the `:EnolaRef` (`resolved: false`). A `target_id` with no fact in the snapshot is an artifact error (none seen in Phase 0) |
| `ENOLA_MATCHES_ROUTE` | client route → server route by `matched_routes[].id` | `confidence` (`verified` \| `probable`); `confidences` list if records disagree. An entry without an `id` is kept in `props_json` only |
| `ENOLA_CALLER` | client route → symbol by `props.caller_id` | Absent where the extractor writes no caller (e.g. Go) |

Rules: a record's `repo` is authoritative for cluster identity. Never deduplicate
by display name or path alone. Row aggregation and ordering are deterministic.
Every node written by a run carries `source_key` and `snapshot_key`, so orphans
can be pruned.

## Run sequence (child)

1. Read the envelope, run the handshake (record the caps), then **probe**: `read_cypher` `MATCH (s:EnolaSnapshot) RETURN count(s)`. Under `extension check` the session ends here, and the child exits 0 without touching Enola.
2. **Resolve the source**: `SOURCE` (absent means the working directory) is canonicalized, classified as directory, config file, or snapshot dir (`--snapshot`), and `source_key` is computed.
3. **Prepare**: generate the snapshot (Phase 3) or open it. Validate and project it into rows in memory (Phase 1).
4. **Replay check**: look up `EnolaSnapshot {snapshot_key}`. If it exists with the same digest, make sure `ENOLA_LATEST` points to it, prune (step 9), report the replay, and exit. A different digest is an error.
5. **Schema**: `update_schema` with `IF NOT EXISTS` unique constraints on `EnolaSource.source_key`, `EnolaSnapshot.snapshot_key`, `EnolaFact.fact_key` and `EnolaRef.ref_key`, plus range indexes on `snapshot_key` and `source_key` for `EnolaFact` and `EnolaRef` and on `EnolaSnapshot.source_key` (verification and pruning). Single-property keys, because Relate's planner seeks a composite `(snapshot_key, enola_id)` index on its first column only, which every fact of a snapshot shares (found in Phase 2).
6. **Write**, as `MERGE` calls chunked against the handshake caps, in this order: facts, refs, resolved relations, ref relations, route matches, caller edges. Re-running after a crash resumes and converges.
7. **Verify**: read back counts scoped by `snapshot_key` (per label and per relationship type) and compare them with the deduplicated prepared rows. A mismatch is a `graph` error, and no marker is written.
8. **Mark**: one `write_cypher` statement merges `:EnolaSource`, creates `:EnolaSnapshot`, and moves `ENOLA_LATEST`.
9. **Prune** (unless `--keep-snapshots`): delete `EnolaFact`, `EnolaRef` and `EnolaSnapshot` nodes with this `source_key` and a different `snapshot_key`, in bounded `DETACH DELETE ... LIMIT n` loops until zero remain. This also removes orphans from failed runs.
10. `report_output` with the summary and starters, then exit 0.

Failure semantics under `per-batch`: a failed run may leave unmarked rows for
its `snapshot_key`. Queries through `ENOLA_LATEST` never see them. A re-run of
the same snapshot completes them, and the next completed snapshot prunes them.
Unrelated graph data is never touched.

## Repository layout

```text
relate-enola/
  Cargo.toml                 # single crate, edition 2021, Apache-2.0
  LICENSE  NOTICE            # ours, plus Enola attribution
  README.md                  # install, usage, flag table, side effects, quarantine workaround
  package/extension.json
  enola.lock.toml            # pinned Enola: version, URLs, archive + binary SHA-256 per target  [Phase 0 ✅]
  build.rs                   # bundled-enola feature: embeds verified binary + notices
  src/
    main.rs                  # thin: envelope -> run -> exit code
    wire/                    # D11 MCP client
    source.rs                # identity + classification
    snapshot.rs              # reader + validation
    project.rs               # rows
    graph.rs                 # static Cypher templates + run sequence
    enola/                   # launcher trait, materializer, private config/HOME, fake launcher
  tests/
    fixtures/                # goldens + GENERATED.md                                         [Phase 0 ✅]
    integration.rs           # drives a real `relate` binary
  notes/                     # Phase 0 report and spike source                                [Phase 0 ✅]
  scripts/
    bump-enola.sh            # D17
    regen-goldens.sh         # D18 (containerized, neutral /fixture paths)
  .github/workflows/
    ci.yml                   # fmt, clippy, unit, integration (relate 1.1.0 + latest), golden drift
    release.yml              # tag-triggered, 4 targets, packs and publishes assets
```

## Phases

### Phase 0: Contract check ✅ (GO)

Done 2026-11-19. See [`notes/enola-contract.md`](notes/enola-contract.md) for the checklist, measurements and spike.
Outputs: `enola.lock.toml`, `tests/fixtures/{cross-repo,orders-api}-v0.5.3/` + `GENERATED.md`,
and `notes/spike/`. The one open spike leg (a real `--github` install) moved to Phase 4.

### Phase 1: Repo scaffold, wire client, projection library ✅

Done 2026-11-19. 56 unit tests (wire, envelope, source, snapshot, projection, hex); fmt and clippy
`-D warnings` clean; passes on macOS arm64 and on Rust 1.87 linux/arm64. `scripts/regen-goldens.sh --check`
reproduces both goldens byte for byte from a clean download.

Results and deviations:

- **End to end on Relate 1.1.0**:
  - `package/extension.json` was added now, not in Phase 2, so the real host could exercise the client.
  - `extension check` passes and stops at the probe, and `install --from` works.
  - `run --snapshot` (a dry run: validate, project, `report_output`, no writes) succeeds on both goldens and the 58k-fact cluster.
  - Staged errors render through Relate's `Error:` line: `[input]`, `[artifact]` (digest mismatch says retry), and `[enola]` (no bundled extractor).
- **Scale (the relate + relate-web cluster)**:
  - Read and validate takes 220 ms; projection takes 213 ms; peak RSS is 397 MB.
  - **50.2 MB of rows** in total: facts 31.9, ref relations 10.4, resolved relations 7.1, refs 0.75. That's down from Phase 0's 84 MB estimate because the store keys are call parameters, not row fields.
  - Rows: 57,979 facts, 20,147 refs, 86,090 resolved and 168,592 ref relations in 15 edge groups, 4 callers. That comes to roughly 335 `write_cypher` calls at 1,000 rows.
- **Raw records are kept as `RawValue` text** (exact original bytes) instead of parsed JSON. That cut peak memory from 705 MB to 397 MB.
- Unknown and future data: unknown relation kinds go to `ENOLA_RELATION {kind}`. Dangling `target_id` is an artifact error. Dangling `matched_routes[].id` or `caller_id` values are skipped and counted (`skipped_prop_refs`). `matched_routes` entries without an `id` are counted and kept only in `props_json`.
- Release binary without Enola: 642 KB.

Original Phase 1 scope:

- `git init`; Apache-2.0 `LICENSE`/`NOTICE`; `Cargo.toml` (deps limited to `serde`, `serde_json`, `sha2`, `tempfile` for tests; no async runtime); `ci.yml` with fmt, clippy and tests.
- **Wire client (D11)**, grown from `notes/spike/src/main.rs`:
  - Envelope read with `SOURCE` optional.
  - `initialize` with the caps recorded, then `notifications/initialized`.
  - Id-correlated JSON-RPC; answers the child didn't ask about are skipped.
  - Typed wrappers for the five tools, row chunking against the caps, and `-32602` as typed errors.
  - A guard so nothing else writes to stdout.
  - Clean exit when the host closes the session at the probe.
- Wire tests: an in-process fake host over a pipe pair covering handshake order, id correlation, chunk boundaries, refusals, and session close at the probe.
- **Reader**: read `receipt.json` first and check `format_version == 1`. Hash the exact `facts.jsonl` bytes against `output_hashes["facts.jsonl"]` (`sha256:<hex>`) and parse those same bytes. Check the raw record count against `fact_count`. On a digest mismatch, report "invalid or mid-publish; retry". Typed structs tolerate additive fields; a 0-fact snapshot is valid.
- **Projector**: the deterministic rows from the graph-model table (R1–R4).
- `scripts/regen-goldens.sh` reproducing `GENERATED.md` in an `ubuntu:24.04` container.
- Unit tests:
  - Golden: cross-repo projection yields the golden tuple, the `probable`/`verified` matches, the unmatched route props, and the expected per-type relationship and ref counts. orders-api yields unprefixed `repo_file`.
  - Validation: malformed JSONL, unknown `format_version`, missing or mismatched digest, count mismatch.
  - Records: duplicate ids with conflicting `line`/`props` (raw retained, convenience props withheld), unknown relation kind → `ENOLA_RELATION`, dangling `target_id` → error, omitted `target_id` → `:EnolaRef`.
  - Edge cases: same-named client and server routes stay separate, 0-fact snapshot, source-key canonicalization (symlinks, trailing slash, `--source-name`).
- Run the projector on a regenerated scale cluster and record row counts and bytes.

**Gate:** `cargo test` passes offline with no Enola and no Relate.

### Phase 2: Snapshot-mode extension (internal milestone M1) ✅

Done 2026-11-19. `src/graph.rs` implements run-sequence steps 4–9, and `src/main.rs` steps 1–10 (generation still stubbed).
There are 59 unit tests, plus **12 integration tests against Relate 1.1.0** (`tests/integration.rs`, `RELATE_BIN`; they skip
cleanly without it), all passing in 4.3 s. CI gained an `integration` job (Relate v1.1.0 and latest × ubuntu and macOS). README written.

Results and deviations:

- **Keys:** single-property `fact_key` and `ref_key` replace the composite constraints. `EXPLAIN` showed the composite index used only for
  `snapshot_key = $sk`, which would make every relationship lookup scan the whole snapshot. `{fact_key: $snapshot_key + '/' + row.src}`
  plans as an exact unique-index seek.
- **Verified on Relate 1.1.0:**
  - `SET n += map`, `MERGE … ON CREATE SET`, `datetime()`, and `b:Label` as an expression all work.
  - The marker statement moves `ENOLA_LATEST` atomically and keeps the first provenance when a snapshot is marked again.
- **`write_cypher` returns counters, not rows**, so the prune loop reads back remaining counts. It cannot stop early, and it fails
  instead of spinning if a delete makes no progress.
- **The host forwards the environment to the child**, so the tests use a test-only failure hook,
  `ENOLA_TO_GRAPH_TEST_FAIL_AFTER=<n tool calls>`. The host prints the summary on one line, so it is written as sentences.
- **Integration coverage:**
  - The golden starters return the exact tuples.
  - Replay is a no-op.
  - An injected failure leaves no marker and nothing visible through `LATEST`, and a re-run converges to a fresh import's
    per-type counts.
  - A failed new snapshot keeps the previous `LATEST` intact.
  - Pruning works; `--keep-snapshots` keeps history and still removes orphans.
  - Identical snapshots from two sources stay isolated, with zero cross-snapshot relationships.
  - `--source-name` works.
  - Unrelated `:Person` data survives every operation.
  - A corrupt digest writes nothing, an empty snapshot gets a marker, and a digest conflict is refused.
- **Scale** (the relate + relate-web snapshot, 58k facts, 20k refs, 255k relationships):

| Operation | Result |
| --- | --- |
| Import | 11.3 s in the child (14 s wall), 361 host calls, 87 MB `.rl` |
| Peak RSS (sampled) | host 327 MB, child 344 MB |
| Replay (unchanged) | 0.2 s, 5 host calls |
| New snapshot + prune of 78k nodes | 9.0 s in the child; **23 s wall**: the host spends ~14 s after the child exits, presumably its final checkpoint |

- Relate "latest" is v1.1.0 today; the CI matrix tests both anyway.

Original Phase 2 scope:

- `package/extension.json`. Implement run sequence steps 1–10 in `graph.rs`, with generation stubbed to "this build has no bundled extractor; use --snapshot".
- README: install, usage, flag table, starter queries, side effects (none in the checkout for directory mode; member caches and providers for cluster configs; `output.root` tip), and the requirements (Relate ≥ 1.1.0, which itself needs glibc ≥ 2.39 on Linux).
- `tests/integration.rs` drives a real `relate` (`RELATE_BIN`; CI installs pinned v1.1.0 and latest), installing the built package with `--from` under a temporary `XDG_CONFIG_HOME`:
  - **Golden import**: import the cross-repo fixture via `--snapshot`. The starter query returns the `web` caller, the `api` handler and `probable`; the unmatched-calls query returns `/api/v2/{}/orders` with `path_unknown`; `net/http.Get` is one `:EnolaRef` with two incoming `ENOLA_CALLS`.
  - **Replay**: re-importing the same snapshot is a no-op and is reported as a replay.
  - **Resume**: kill the child mid-write, re-run, and the snapshot converges to the same counts.
  - **Prune**: import A then B for the same source. Only B remains (refs included), `ENOLA_LATEST` points to B, and orphans from a killed run are gone. With `--keep-snapshots`, both remain.
  - **Isolation**: two sources importing the *same* fixture (same `snapshot_id`) produce zero cross-snapshot edges; pruning one leaves the other intact.
  - **Unrelated data**: pre-seeded non-Enola data survives every case.
  - **Corrupt digest**: exits nonzero at the `artifact` stage before any write.
  - **0-fact snapshot**: imports a marker with zero facts and reports it.
  - `relate extension check` passes without preparing anything.
- Measure Relate import time and peak memory (host and child) on the scale cluster's snapshot.
- CI golden-drift job: download pinned Enola, run `scripts/regen-goldens.sh`, compare `facts.jsonl` bytes and `snapshot_id`.

**Gate:** M1 merged to `main`. No tag (D3).

### Phase 3: Bundled generation

- `scripts/bump-enola.sh` (D17) updates `enola.lock.toml` and regenerates the goldens.
- Cargo feature `bundled-enola`. `build.rs` reads `ENOLA_BIN` and `ENOLA_NOTICES`, checks `ENOLA_BIN` against `binary_sha256` for the target, and fails clearly if the feature is on and the inputs are absent. Without the feature, everything builds and tests run with no upstream asset.
- Materialize (D9) to `<cache>/enola/<sha256>/enola`: temp file in the same dir, verify, `chmod 0700`, atomic rename; reuse on hash match (proven in the spike: 127–211 ms).
- Launch only after the probe: absolute path, argument vector, private working dir with no `mcp-arch.yaml`, null stdin, bounded stdout and stderr capture, and `HOME=<cache>/home ENOLA_NO_UPDATE_CHECK=1 ENOLA_NO_PROMPTS=1` (D20).
- Directory mode: write the private config (YAML-escaped absolute path), run `--generate <cfg> --no-cluster`, and read `<cache>/out/<absolute repo path>/.enola/`.
- Cluster mode: `--generate <abs config> --out <cache>/clusters/<sha(source_key)>`. **Open (found in Phase 1):** a config file can also be a *single-repo* config (`repo:` with no `repos:`), and Enola refuses `--out` for that (exit 1). Detect it before launching, without a YAML dependency: either a minimal top-level key scan for `repos:`, or retry without `--out` on that specific refusal and read the repo's output location from Enola's `Output:` line. Decide and test in Phase 3.
- Trust artifacts only after exit 0. "Snapshot unchanged" (exit 0, stdout `Snapshot unchanged:`) reuses the artifacts at the expected path. Never ingest after a failed run, even when older artifacts exist (Enola leaves them in place).
- A launcher trait with a fake that writes fixture artifacts or fails on demand.
- Tests: both modes through the fake; a failure with stale artifacts imports nothing; the unchanged path; `--snapshot` and live generation of the same fixture give equal projections; a real `bundled-enola` run proves the checkout and the real `~/.enola` are untouched.

**Gate:** a local `bundled-enola` build runs `relate run enola-to-graph` on a real repo and on the cross-repo fixture with Enola absent from `PATH`, and neither the checkout nor `~/.enola` changes.

### Phase 4: Release pipeline, proof, and v0.1.0

- `release.yml`, triggered by a `v*` tag:
  - Check that the tag, `Cargo.toml` and `extension.json` versions match.
  - Matrix:

| Target | Runner | Asset suffix |
| --- | --- | --- |
| `x86_64-unknown-linux-musl` | `ubuntu-latest` | `linux-amd64` |
| `aarch64-unknown-linux-musl` | `ubuntu-24.04-arm` | `linux-arm64` |
| `x86_64-apple-darwin` | `macos-15-intel` | `darwin-amd64` |
| `aarch64-apple-darwin` | `macos-latest` | `darwin-arm64` |

  - Each leg downloads its Enola archive from `enola.lock.toml`, verifies the archive and binary SHA-256, extracts only the binary plus `LICENSE`/`NOTICE`, and builds with `--features bundled-enola`.
  - Darwin legs run `codesign --force -s -` on the output and then `codesign -v` (D12).
  - Each leg smoke-tests natively: `extension check`, `install --from`, `run --snapshot` on the golden fixture, and a live `run` on the fixture.
  - A pack job writes `enola-to-graph-<version>-<os>-<arch>.tar.gz` (GNU tar, exactly `extension.json` plus the executable with its exec bit).
  - GitHub release with the four assets. Immutable releases on; tags are never moved.
- **First pre-release (`v0.1.0-rc.1`) closes the Phase 0 `--github` leg**: install via `--github` on all four platforms against Relate 1.1.0 and latest, and confirm the trust-store pin and that a moved tag is refused.
- Release gates:

| Check | Required result |
| --- | --- |
| `--github` install | Succeeds on all four platforms; the pin is recorded |
| Clean offline run | With Enola absent from `PATH` and networking blocked after install: a real repo and the cross-repo fixture produce queryable `.rl` files on all four platforms |
| Evidence | Golden tuple, `probable` confidence, unmatched dynamic call, `:EnolaRef` targets, receipt-matching snapshot record |
| Lifecycle | Replay no-op, resume after kill, prune and `--keep-snapshots`, isolation, unrelated data preserved |
| Side effects | Checkout and real `~/.enola` untouched in directory mode |
| Failure | Enola failure with stale artifacts, corrupt digest, host write failure mid-import: each exits nonzero, writes no marker, and leaves the prior `LATEST` intact |
| Scale | Representative cluster: Enola time, import time, peak RSS (host and child), wire bytes, `.rl` size, counts; recorded with versions |

- Single-repo git enrichment demo (D4) in the README: join `handler.repo_file` to `git-to-graph`'s `(:File {path})` in a store holding only that repo's history, traverse `:TOUCHED` to `:Commit`, and check against `git log -- <file>`.
- Release notes: embedded Enola version, minimum Relate version, quarantine workaround for manual downloads.
- Tag `v0.1.0`.

## Starter queries

Literal Cypher (`report_output` starters take no parameters):

```cypher
// Cross-repo HTTP calls resolved to handlers, latest snapshot per source
MATCH (:EnolaSource)-[:ENOLA_LATEST]->(s:EnolaSnapshot)
MATCH (client:EnolaFact {snapshot_key: s.snapshot_key})
      -[m:ENOLA_MATCHES_ROUTE]->(route:EnolaFact)-[:ENOLA_HANDLED_BY]->(handler:EnolaFact)
WHERE client.repo <> route.repo
RETURN client.repo AS caller_repo, client.repo_file AS caller_file, client.line AS caller_line,
       route.method AS method, route.name AS path,
       handler.repo AS handler_repo, handler.name AS handler, m.confidence AS confidence
ORDER BY caller_repo, caller_file, caller_line
LIMIT 50
```

```cypher
// HTTP calls no loaded service answers
MATCH (:EnolaSource)-[:ENOLA_LATEST]->(s:EnolaSnapshot)
MATCH (c:EnolaFact {snapshot_key: s.snapshot_key, kind: 'route', unmatched_by_server: true})
RETURN c.repo, c.repo_file, c.line, c.method, c.name, c.unmatched_reason
ORDER BY c.repo, c.repo_file, c.line
LIMIT 50
```

```cypher
// Most-called targets outside the snapshot (external APIs, stdlib)
MATCH (:EnolaSource)-[:ENOLA_LATEST]->(s:EnolaSnapshot)
MATCH (f:EnolaFact {snapshot_key: s.snapshot_key})-[:ENOLA_CALLS]->(r:EnolaRef)
RETURN r.target AS target, count(DISTINCT f) AS callers
ORDER BY callers DESC, target
LIMIT 25
```

## Risks and open items

- **Host contract drift**: our own client (D11) tracks the spec by hand; the CI job against the latest Relate release is the early warning.
- **Enola contract drift on upgrade**: the golden-drift job and the D17 bump flow catch it. Enola v0.5.3 was published one day before Phase 0 ran, so expect frequent releases.
- **Docs vs behavior**: Enola's docs say clusters write nothing into members, but v0.5.3 writes extractor caches. Trust the observed behavior and re-check on each bump.
- **Manual downloads on macOS**: a quarantined browser download is blocked without notarization; documented.
- **Corporate endpoint policy** may block executing from cache dirs; `--snapshot` is the documented fallback.
- **Prune cost on large stores**: bounded delete loops; measured in Phase 4.
- **Future work** (out of scope): `insights.json`/`flows.json`/`docs.jsonl`, per-run history beyond `--keep-snapshots`, injecting `output.root` into user cluster configs, Rubydex provisioning, repo-qualified `git-to-graph` identity, Windows, notarization, upstream first-party status.

## References

- Enola v0.5.3 at `f0974e49`: `docs/INTEGRATING.md`, `docs/schema/facts.md`, `docs/schema/receipt.md`, `docs/CLI.md` (`--generate`, `--no-cluster`, `--force`, `--out`), `ARCHITECTURE.md` (output config, config resolution), `examples/cross-repo`
- Relate: `docs/api/extensions/spec.md` (Transport contract, caps, check verb, GitHub pin tier), `docs/api/extensions/install.md`, `extensions/relate-git-to-graph/` (design reference)
- Phase 0: `notes/enola-contract.md`, `notes/spike/`, `tests/fixtures/GENERATED.md`, `enola.lock.toml`
