# enola-to-graph

A [Relate](https://relatedb.ai) command extension that turns an
[Enola](https://github.com/enola-labs/enola) architecture snapshot into a
persistent, queryable Relate graph: modules, symbols, routes, the calls and
imports between them, and the HTTP calls Enola resolved *across repositories*.

> **Status: pre-release.** Importing an existing Enola snapshot (`--snapshot`)
> works today. One-command generation with a bundled Enola binary, and the
> `--github` release, arrive with v0.1.0 (see [PLAN.md](PLAN.md)).

## Requirements

- Relate **1.1.0 or newer** (on Linux, Relate itself needs glibc 2.39+, e.g. Ubuntu 24.04).
- For `--snapshot`: a snapshot written by Enola **0.5.x** (`format_version` 1).

## Install

Until the first tagged release, build and install from a checkout:

```sh
cargo build --release
mkdir -p /tmp/enola-to-graph-pkg
cp package/extension.json target/release/enola-to-graph /tmp/enola-to-graph-pkg/
relate extension install enola-to-graph --from /tmp/enola-to-graph-pkg
```

From v0.1.0:

```sh
relate extension install enola-to-graph --github johnymontana/relate-enola@v0.1.0
```

## Usage

```sh
# Import a snapshot Enola already wrote (a directory holding receipt.json + facts.jsonl)
enola --generate ./cluster.yaml --out ./enola-out
relate run enola-to-graph ./enola-out --snapshot -d ./architecture.rl

# Keep one stable identity across checkouts or CI paths
relate run enola-to-graph ./enola-out --snapshot --source-name payments -d ./architecture.rl
```

| Flag / positional | Kind | Default | Meaning |
| --- | --- | --- | --- |
| `SOURCE` (positional) | path | current directory | With `--snapshot`: an Enola snapshot directory. (From v0.1.0 without `--snapshot`: a repository directory or an Enola cluster config.) |
| `--snapshot` | boolean | `false` | Import an existing snapshot directory instead of running Enola |
| `--source-name NAME` | string | (path) | Identify the source by name instead of its canonical path |
| `--keep-snapshots` | boolean | `false` | Keep this source's earlier snapshots instead of replacing them |
| `-d`, `--database PATH` | Relate host flag | in-memory | The store to write. Owned by Relate; pass it for a persistent graph |

Each run validates the snapshot against its receipt (format version, facts
digest, record count), writes it, verifies the written graph, and only then
marks it complete. Re-running an unchanged snapshot is a no-op. Importing a
new snapshot of the same source replaces the old one after the new one is
complete.

## The graph

| Element | What it is |
| --- | --- |
| `(:EnolaSource)` | What you pointed at (`source_key` = `path:<canonical path>` or `name:<source-name>`) |
| `(:EnolaSnapshot)` | One completed import: `snapshot_id`, `enola_version`, `facts_digest`, counts, `coverage_json` (Enola's quality block), `provenance_json` (git state per repo) |
| `(:EnolaSource)-[:ENOLA_LATEST]->(:EnolaSnapshot)` | The current snapshot of each source. **Query through it**, so you never see an incomplete import |
| `(:EnolaFact)` | One Enola fact: `kind` (`symbol`, `module`, `route`, `dependency`, `service`, …), `repo`, `name`, `repo_file`, `line`, `end_line`, contract props (`symbol_kind`, `method`, `role`, `unmatched_by_server`, …), and all props as `props_json` |
| `(:EnolaRef)` | A relation target outside the snapshot (stdlib, third-party, ambiguous names), shared by everything that references it |
| `ENOLA_CALLS`, `ENOLA_IMPORTS`, `ENOLA_DECLARES`, `ENOLA_HANDLED_BY`, `ENOLA_DEPENDS_ON`, … | Enola's relations, one type per kind; `resolved: true` to a fact, `false` to a ref. Unknown kinds become `ENOLA_RELATION {kind}` |
| `ENOLA_MATCHES_ROUTE {confidence}` | A client call site matched to the server route it reaches (`verified` or `probable`) |
| `ENOLA_CALLER` | A client call site to the function that makes the call |

Every Enola node carries `source_key` and `snapshot_key`, so snapshots of
different sources never share nodes, even when their contents are identical.

## Starter queries

The summary of each run prints these:

```cypher
// Cross-repo HTTP calls resolved to their handlers
MATCH (:EnolaSource)-[:ENOLA_LATEST]->(s:EnolaSnapshot)
MATCH (client:EnolaFact {snapshot_key: s.snapshot_key})
      -[m:ENOLA_MATCHES_ROUTE]->(route:EnolaFact)-[:ENOLA_HANDLED_BY]->(handler:EnolaFact)
WHERE client.repo <> route.repo
RETURN client.repo, client.repo_file, client.line, route.method, route.name,
       handler.repo, handler.name, m.confidence
ORDER BY client.repo, client.repo_file, client.line LIMIT 50

// HTTP calls no loaded service answers
MATCH (:EnolaSource)-[:ENOLA_LATEST]->(s:EnolaSnapshot)
MATCH (c:EnolaFact {snapshot_key: s.snapshot_key, kind: 'route', unmatched_by_server: true})
RETURN c.repo, c.repo_file, c.line, c.method, c.name, c.unmatched_reason LIMIT 50

// Most-called targets outside the snapshot
MATCH (:EnolaSource)-[:ENOLA_LATEST]->(s:EnolaSnapshot)
MATCH (f:EnolaFact {snapshot_key: s.snapshot_key})-[:ENOLA_CALLS]->(r:EnolaRef)
RETURN r.target, count(DISTINCT f) AS callers ORDER BY callers DESC LIMIT 25
```

## Good to know

- **Writes commit in batches.** A run that fails part-way leaves unmarked rows
  that nothing reached through `ENOLA_LATEST` shows. Re-running completes
  them, and the next completed snapshot of that source removes them. The
  previous snapshot stays current until a new one is complete.
- **Replacing a snapshot deletes its nodes.** Relationships *you* add to
  `EnolaFact` nodes go with them. Use `--keep-snapshots` to keep history.
- **Your other data is never touched.** Everything this extension writes has an `Enola*` label.
- **Scale** (measured, Apple M-series): a two-repository cluster of about 5,000
  files (58k facts, 255k relationships) imports in about 11 s, with about
  330 MB peak for both Relate and the extension, giving an 87 MB `.rl`.
  Re-running it unchanged takes 0.2 s.
- **Errors name their stage**: `[input]`, `[enola]`, `[artifact]` (the snapshot
  is invalid; a digest mismatch usually means Enola was still writing, so retry),
  or `[graph]` (the host refused a write).

## Development

```sh
cargo test                                            # unit tests, offline
RELATE_BIN=$(command -v relate) cargo test --test integration   # against a real relate
cargo run --release --example project_stats -- <snapshot-dir>   # size a snapshot
scripts/regen-goldens.sh --check                      # golden drift (needs docker)
```

- [PLAN.md](PLAN.md): decisions and phases.
- [notes/enola-contract.md](notes/enola-contract.md): the observed Enola contract.
- [tests/fixtures/GENERATED.md](tests/fixtures/GENERATED.md): how the goldens were generated.

## License

Apache-2.0 (see [LICENSE](LICENSE)). Release builds embed Enola, which is
Apache-2.0 with MIT-licensed vendored components; see [NOTICE](NOTICE) and
[third_party/enola/](third_party/enola/).
