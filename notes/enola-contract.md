# Phase 0: Enola contract check and packaging spike

Date: 2026-11-19. Enola **v0.5.3** (tag commit `f0974e49`, published 2026-11-18,
`extractor_version` `v650`). Host: **Relate 1.1.0**. Everything below was observed
by running the pinned binaries unless marked *(docs)*.

**Verdict: GO.** Nothing blocks the plan. Several findings change the design; they
are listed under [Plan revisions](#plan-revisions) and need sign-off before Phase 1.
One spike leg (a real `--github` install) is still open; it needs a GitHub release
to install from.

## Checklist

| # | Item | Result |
| --- | --- | --- |
| 1 | License permits redistribution | ✅ Apache-2.0 (Copyright Dejan Menges). `NOTICE` lists vendored MIT tree-sitter runtime and grammars. Each archive ships `LICENSE` + `NOTICE`; we must ship both alongside our own |
| 2 | Archive URLs, checksums, sizes, signatures | ✅ Recorded in `enola.lock.toml`, verified three ways (`.sha256` file, GitHub `digest`, local hash). Binaries 58–61 MB, archives 13–15 MB. darwin-arm64 is ad-hoc linker-signed; **darwin-amd64 has no signature**; Linux binaries are **dynamically linked against glibc** (cgo), floor **GLIBC_2.17** |
| 3 | Generate `examples/cross-repo` | ✅ `tests/fixtures/cross-repo-v0.5.3/` (see `GENERATED.md`) |
| 4 | Receipt fields | ✅ See [Receipt](#receipt) |
| 5 | Facts shape | ✅ See [Facts](#facts); several plan assumptions corrected |
| 6 | Golden tuple frozen | ✅ In `tests/fixtures/GENERATED.md` |
| 7 | Single-repo mode | ✅ `output.root` keeps everything out of the checkout; repo-local `mcp-arch.yaml` is **not** read with an explicit config; `--no-cluster` is required. See [Generation](#generation) |
| 8 | "Snapshot unchanged" | ✅ Exit 0, stdout `Snapshot unchanged:`, artifacts untouched and current |
| 9 | Failure behavior | ✅ Exit 1, reason on the last stderr lines, output dir untouched (stale artifacts stay) |
| 10 | Representative large cluster | ✅ See [Scale](#scale) |
| S1 | Spike: embed, check, install `--from`, run | ✅ darwin-arm64 (native), darwin-amd64 (Rosetta), linux-arm64 (Ubuntu 24.04 container) |
| S2 | Spike: `--github` install from a real release | ⏳ Open: needs a GitHub repo and release (see the end of this file) |

## Receipt

Observed v0.5.3 receipt keys: `snapshot_id`, `format_version` (1), `enola_version`
(`"0.5.3"`), `extractor_version`, `generated_at`, `duration` (empty for clusters),
`repo_path` (empty for clusters), `git`, `extractors`, `explainers`, `renderers`,
`config_hash`, `ignore_glob_hash`, `output_hashes`, `fact_count`, `insight_count`,
`quality`, `members` (clusters only).

- **Hash encoding:** `"sha256:<64 lowercase hex>"`, over the exact file bytes. Verified for every output.
- **`fact_count` is the raw JSONL record count** (58,365 records → 57,979 distinct ids in the scale run).
- **`git` (single repo):** `{ref, commit, dirty, remote}` with `remote` normalized (`github.com/example-org/orders-api`). For a cluster each `members[]` entry carries `{repo, repo_path, git, extractors, files_seen, files_parsed, files_skipped, dirs_skipped, parse_errors}`.
- **`quality.coverage`:** `{services_total, coverage_gaps, unresolved_edges}` for clusters.
- **`snapshot_id` excludes `output.*` and `history.*` config and source location.** The same sources produced the same id from different paths, platforms and `output`/`history` settings. So **two different sources can share a `snapshot_id`**, which confirms scoping `snapshot_key` by `source_key`.
- `enola --version` prints to **stderr**. Use the receipt's `enola_version`.

## Facts

Field names match `docs/schema/facts.md` at the tag. Corrections to the plan's model:

1. **Relation kinds: 14, not 5.** `declares`, `imports`, `calls`, `implements`, `depends_on`, `instantiates`, `injects`, `has_method`, `handled_by`, `implemented_by`, `publishes`, `subscribes`, `exercises`, `names`. All get a static template; anything else falls back to `ENOLA_RELATION`.
2. **The unresolved dynamic call is not an unresolved relation.** It is a client `route` fact with `props.unmatched_by_server: true` and `props.unmatched_reason: "path_unknown"`, and it has no relations beyond `declares`. Server routes carry `props.matched_by_clients`.
3. **Relations without `target_id` are mostly external targets** (`net/http.Get`, `github.com/gorilla/mux`), not ambiguous ones. In the scale run: 168,592 such relations, but only 25,384 distinct `(kind, target)` pairs and 20,147 distinct targets.
4. **`matched_routes`** entries are `{repo, name, file, method, confidence, id}`. `confidence` is `verified` or `probable`. The golden `{id}` call is `probable`; the literal path is `verified`.
5. **`caller` / `caller_id`** on client routes link a call to the function making it (TS, Kotlin, Swift, Java, Dart and Python only; the Go fixture has none). The scale run had 4 of 5.
6. **Multi-repo `file`** is `"<repo>/<path>"` with `repo_prefixed: true`; a module's file is `"api/."`. `service` facts and synthetic cross-repo `dependency` facts have no `file` and no `repo_prefixed`.
7. **Duplicate ids are real but rare:** 307 of 57,979 ids (0.5%) have 2–8 records. *All* 307 conflict (305 on `line`, 110 on `props`, 100 on `end_line`), across `dependency` (205), `symbol` (100) and `module` (2). This supports D6.
8. **No dangling `target_id`s** in either run, and every record has an `id`.
9. **Non-contract props are large and plentiful** (`handler`, `handler_target`, `language`, `cyclomatic`, `body_hash`, `doc`, …): 9.9 MB of props JSON in the scale run. Nested props seen: `matched_routes`, `edge_coverage`.
10. A snapshot can legitimately contain **0 facts** (a repo with no code), and Enola exits 0. The importer must handle it.

## Generation

- **Config resolution** *(docs + observed)*: Enola reads `mcp-arch.yaml` from its **working directory**, or from beside its executable when the binary is not on `PATH`. An explicit config path wins. With our private config, a planted `mcp-arch.yaml` in the scanned repo (narrowed extractors plus a provider command) was **ignored**: the provider never ran.
- **Directory mode recipe** (tested):
  ```yaml
  repo: "<absolute repo path>"
  output: {dir: ".enola", root: "<cache>/out"}
  history: {enabled: false}
  ```
  `enola --generate <private config> --no-cluster`, run from a private working directory. Artifacts land at `<root>/<absolute repo path>/.enola/` (the path mirrored under root), extractor cache included. **The checkout is untouched.**
- **`--no-cluster` is required.** Without it, a `SOURCE` folder holding two or more git repos is auto-clustered, Enola **writes a `cluster.yaml` into the user's folder**, and output goes to `~/.enola/graphs/`.
- **Cluster mode** (`--generate cluster.yaml --out DIR`): `DIR` must be outside every member. **Members still get `.enola/extractor_cache.json` written into them**, despite the cluster section of the docs, unless the user's config sets `output.root`, which relocates those caches too (tested). Re-running with the same `--out` is a ~0.3 s `Snapshot unchanged` no-op, so `--out` should be a **stable per-source cache dir**, not a temp dir.
- **`--out` on a single repo** is refused (exit 1). An `output.dir` outside the repo is refused.
- **`$HOME` side effects (every run):** `~/.enola/receipt.json` and `~/.enola/graphs/<key>.json` are always written. History goes under `~/.enola/graphs/<key>/history` unless `history.enabled: false`. There is no environment variable to relocate `~/.enola`. Running with `HOME` pointed at an empty private dir works, and git provenance is still captured.
- **Stderr is verbose:** 17 KB for the scale run, ~100 log lines even for the fixture. Keep a bounded tail for diagnostics.
- **Exit codes:** 0 success (including unchanged and 0-fact snapshots); 1 for a missing repo, invalid YAML, a missing config, a refused `--out` or `output.dir`.

## Scale

Cluster of shallow clones of `relatedb/relate` (Rust, 3,905 files) and `relatedb/relate-web`
(TS, 1,106 files), on an M-series Mac with darwin-arm64:

| Measure | Value |
| --- | --- |
| Enola generation | 4.3 s wall, 470 MB peak RSS |
| Re-run, unchanged | 0.3 s |
| Records / distinct facts | 58,365 / 57,979 |
| `facts.jsonl` | 45.1 MB |
| Resolved edges (deduplicated) | 86,090 |
| Unresolved relations (per source) / distinct targets | 168,592 / 20,147 |
| Estimated row JSON to send | **~84 MB** (facts 34.3, resolved edges 16.5, ref nodes 3.4, unresolved edges 29.3), ~335 write calls at 1,000 rows each |

An ordinary two-repo cluster already reaches ~65% of `whole-import`'s 128 MiB cap
before JSON-RPC framing. **D1 (per-batch) is confirmed.** Relate import time and
peak memory are measured in Phase 2 once there is an importer.

## Platform findings

- **Relate 1.1.0 itself needs glibc ≥ 2.39** (Ubuntu 24.04+); it fails on Debian 12. Enola needs only 2.17, so Enola is never the binding constraint on a machine that runs Relate. Alpine can't run Relate's gnu binary either. **D14 (musl) gains nothing over gnu, but costs nothing**; keep it.
- **On Alpine, Enola fails with a misleading `not found` (exit 127)** unless `gcompat` is installed. The extension maps "exec ENOENT on an existing file" to an actionable message.
- **macOS signatures:** the Rust linker ad-hoc signs **arm64** builds automatically, but an **x86_64 build cross-compiled on arm64 is unsigned**, and Enola's own darwin-amd64 binary is unsigned. Intel Macs don't require signatures, and both ran fine under Rosetta. The release job should **explicitly ad-hoc sign both darwin outputs** (`codesign --force -s -`) and then verify.
- The extracted Enola keeps its valid signature. Only `com.apple.provenance` is set (not quarantine), so Gatekeeper never assesses it.

## Spike results (`notes/spike/`)

A throwaway extension embedding the real Enola via `include_bytes!`, with a hand-written MCP client.

| Leg | check | install `--from` | run | Cache write |
| --- | --- | --- | --- | --- |
| darwin-arm64, native, Relate 1.1.0 | ✅ | ✅ | ✅ | 163 ms first, 211 ms reuse (re-hash) |
| darwin-amd64, Rosetta | ✅ | ✅ | ✅ | 175 ms |
| linux-arm64 musl, Ubuntu 24.04 container | ✅ | ✅ | ✅ | 127 ms |
| linux-amd64 | not run locally; Phase 4 CI on native runner | | | |

Confirmed:

- **`extension check` never reaches Enola.** The session ends at the probe `read_cypher`, the child exits 0, and no cache dir is created.
- **The envelope key is the declared positional name verbatim: `"SOURCE"`**, not lowercased. It is **absent** when `SOURCE` is omitted (`{"snapshot":true}`). (The spec's `{"dir": "."}` example misled the plan; a flag named `source` would not collide. `--source-name` stays.)
- Package size: the extension executable is ~59 MB (Enola plus ~1 MB of our code).
- The cache reuse path re-hashes 58 MB (~50–200 ms). Acceptable; a size+mtime stamp could skip it later.

## Plan revisions

Proposed changes to PLAN.md (pending sign-off):

| # | Change | Why |
| --- | --- | --- |
| R1 | Replace per-source `:EnolaUnresolvedRef` with shared **`:EnolaRef {snapshot_key, target}`** nodes, reached by the *same* typed relationship (`ENOLA_CALLS` to an `:EnolaRef`) with `resolved: false` on the edge | 20k nodes instead of 168k; "who calls `net/http.Get`" becomes one hop; external targets aren't mislabeled as unresolved |
| R2 | Unmatched HTTP calls are expressed through `EnolaFact` props `unmatched_by_server` and `unmatched_reason`; the starter query for unresolved calls uses them | That's how Enola v0.5.3 encodes them (finding 2) |
| R3 | Static templates for all 14 relation kinds, plus `ENOLA_RELATION` fallback; add **`ENOLA_CALLER`** (client route → symbol) from `caller_id`, alongside `ENOLA_MATCHES_ROUTE` | Findings 1, 5 |
| R4 | Store **`props_json`** (deterministic) on every `:EnolaFact`; promote contract props to top-level properties; `raw_records` only for multi-record or conflicting ids (D6 unchanged) | Otherwise non-contract props are silently lost (finding 9) |
| R5 | **D7 resolved:** directory mode sets `output.root` to `<cache>/out` → nothing written to the checkout. Cluster mode: README tells users to set `output.root` in their config to keep member caches out of their repos | Generation findings |
| R6 | Directory mode keeps `--no-cluster`; cluster `--out` = stable `<cache>/clusters/<sha(source_key)>` | Generation findings |
| R7 | Decide `$HOME` handling for the Enola subprocess (see question below) | `~/.enola` side effects |
| R8 | Release job explicitly ad-hoc signs darwin outputs; drop the Alpine claims; README states Relate's own glibc ≥ 2.39 floor | Platform findings |
| R9 | Read `enola_version` from the receipt; handle 0-fact snapshots; envelope key `SOURCE` (absent → current dir) | Receipt, facts and spike findings |

## Open: `--github` leg

`relate extension install --github OWNER/REPO@TAG` needs a real GitHub release with
correctly named assets (`<name>-<version>-<os>-<arch>.tar.gz`, two members, GitHub
`digest` recorded). `johnymontana/relate-enola` doesn't exist yet. Options: a scratch
repo with a pre-release of the spike, or defer this leg to the first real pre-release
in Phase 4.
