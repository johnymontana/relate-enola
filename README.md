# enola-to-graph

A [Relate](https://relatedb.ai) command extension that turns a repository, or
a cluster of repositories, into a persistent, queryable Relate graph of its
architecture: modules, symbols, routes, the calls and imports between them,
and the HTTP calls resolved *across repositories*. It works by running
[Enola](https://github.com/enola-labs/enola), bundled inside the extension,
then importing Enola's snapshot.

> **Status: pre-release.** Generation with the bundled Enola and `--snapshot`
> imports both work from a local build. The `--github` release arrives with
> v0.1.0 (see [PLAN.md](PLAN.md)).

## Requirements

- Relate **1.1.0 or newer** (on Linux, Relate itself needs glibc 2.39+, e.g. Ubuntu 24.04).
- macOS or Linux, x86_64 or arm64. Nothing else: Enola ships inside the extension.

## Install

From v0.1.0:

```sh
relate extension install enola-to-graph --github johnymontana/relate-enola@v0.1.0
```

Until then, build with the bundled Enola from a checkout:

```sh
export $(scripts/fetch-enola.sh | tail -1)        # downloads + verifies the pinned Enola
cargo build --release --features bundled-enola
mkdir -p /tmp/enola-to-graph-pkg
cp package/extension.json target/release/enola-to-graph /tmp/enola-to-graph-pkg/
relate extension install enola-to-graph --from /tmp/enola-to-graph-pkg
```

## Usage

```sh
# One repository (the graph should live outside the scanned tree)
relate run enola-to-graph . -d ../architecture.rl

# A cluster: an Enola config listing at least two repositories
relate run enola-to-graph ./cluster.yaml -d ./architecture.rl

# A snapshot Enola already wrote (a directory with receipt.json + facts.jsonl)
relate run enola-to-graph ./enola-out --snapshot -d ./architecture.rl

# A stable identity across checkouts or CI paths
relate run enola-to-graph . --source-name payments -d ./architecture.rl
```

| Flag / positional | Kind | Default | Meaning |
| --- | --- | --- | --- |
| `SOURCE` (positional) | path | current directory | A repository directory, an Enola config file (a cluster), or with `--snapshot` a snapshot directory |
| `--snapshot` | boolean | `false` | Import an existing snapshot directory instead of running Enola |
| `--source-name NAME` | string | (path) | Identify the source by name instead of its canonical path |
| `--keep-snapshots` | boolean | `false` | Keep this source's earlier snapshots instead of replacing them |
| `-d`, `--database PATH` | Relate host flag | in-memory | The store to write. Owned by Relate; pass it for a persistent graph |

| Environment | Meaning |
| --- | --- |
| `ENOLA_TO_GRAPH_CACHE_DIR` | Absolute path for the extension's cache (default below) |

Each run generates or validates the snapshot (format version, facts digest,
record count), writes it, verifies the written graph, and only then marks it
complete. Re-running unchanged code is fast: Enola reports nothing changed
and the import is a no-op. A new snapshot of the same source
replaces the old one once it is complete.

A directory is always indexed as **one** repository, even a folder that
contains several git repositories. To index a cluster, pass an Enola config
that lists the repositories (`enola cluster init <dir>` writes one; the config
format is in Enola's docs).

## What it writes, and where

- **Into your repository: nothing**, in directory mode. Enola's output and incremental cache live in the extension's cache.
- **Cluster configs are yours**, so Enola follows them, including any `providers` they configure, which run their own commands.
  Enola also writes each member's `.enola/extractor_cache.json` into the member. To keep members clean, set
  `output: {root: /some/dir}` in your cluster config.
- **The extension's cache** is `~/Library/Caches/relate-enola` on macOS, or `$XDG_CACHE_HOME/relate-enola`
  (default `~/.cache/relate-enola`) on Linux. It holds:
  - the unpacked Enola binary, with its LICENSE and NOTICE;
  - Enola's private home (Enola runs with `HOME` set there, so your own `~/.enola` is never touched);
  - outputs.

  It is safe to delete; the next run rebuilds it.
- **The graph store** is the one you pass with `-d`.

## The graph

| Element | What it is |
| --- | --- |
| `(:EnolaSource)` | What you pointed at (`source_key` = `path:<canonical path>` or `name:<source-name>`; `source_kind` = `repo`, `cluster` or `snapshot`) |
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
  previous snapshot stays current until a new one is complete, and a failed
  Enola run never imports anything.
- **Replacing a snapshot deletes its nodes.** Relationships *you* add to
  `EnolaFact` nodes go with them. Use `--keep-snapshots` to keep history.
- **Your other data is never touched.** Everything this extension writes has an `Enola*` label.
- **Scale** (measured, Apple M-series): a two-repository cluster of about 5,000
  files (58k facts, 255k relationships) takes about 20 s on the first run
  (Enola about 5 s, import about 11.5 s), with about 330 MB peak for both
  Relate and the extension, giving an 87 MB `.rl`. Re-running unchanged code
  takes about 2 s: Enola's change check plus re-validating its 45 MB snapshot.
- **Errors name their stage**:
  - `[input]`: a bad `SOURCE`.
  - `[enola]`: Enola failed; its own error is quoted.
  - `[artifact]`: the snapshot is invalid. A digest mismatch usually means Enola was still writing, so retry.
  - `[graph]`: the host refused a write.
- **Alpine** can't run Relate's own Linux binary. Where the bundled Enola can't
  start for lack of glibc, the error says so; use `--snapshot` with an Enola run elsewhere.
- **macOS, manual downloads:** `relate extension install` is not affected. If
  you download a release tarball in a browser and unpack it by hand, macOS
  quarantines it; run `xattr -d com.apple.quarantine enola-to-graph`.

## Development

```sh
cargo test                                                     # unit tests, offline
RELATE_BIN=$(command -v relate) cargo test --test integration  # against a real relate
export $(scripts/fetch-enola.sh | tail -1)
RELATE_BIN=$(command -v relate) cargo test --features bundled-enola --test integration  # + live generation
cargo run --release --example project_stats -- <snapshot-dir>  # size a snapshot
scripts/regen-goldens.sh --check                               # golden drift (needs docker)
scripts/bump-enola.sh <version>                                # pin a new Enola release
```

- [PLAN.md](PLAN.md): decisions and phases.
- [notes/enola-contract.md](notes/enola-contract.md): the observed Enola contract.
- [tests/fixtures/GENERATED.md](tests/fixtures/GENERATED.md): how the goldens were generated.

## License

Apache-2.0 (see [LICENSE](LICENSE)). Release builds embed Enola, which is
Apache-2.0 with MIT-licensed vendored components; see [NOTICE](NOTICE) and
[third_party/enola/](third_party/enola/). The `tests/fixtures/src/cross-repo`
sources are Enola's own example (Apache-2.0).
