# Golden fixtures: how they were generated

Generated 2026-11-19 with **Enola v0.5.3** (`enola-0.5.3-linux-arm64`, archive
SHA-256 `b84b054d547dd2e71f3d4fdc433e74cf20ef8aa9c9161cfabb04a9f0f5766d62`),
inside an `ubuntu:24.04` linux/arm64 container. Sources were mounted at
`/fixture/src/...` so the receipts carry neutral paths.

Only the two contract artifacts each run consumes are committed: `receipt.json`
and `facts.jsonl`. They are unmodified Enola output.

Environment for every run: `HOME=/tmp/h ENOLA_NO_UPDATE_CHECK=1 ENOLA_NO_PROMPTS=1`,
working directory `/tmp/run` (which contains no `mcp-arch.yaml`).

## cross-repo-v0.5.3

Source: `examples/cross-repo` from `enola-labs/enola` at tag `v0.5.3`
(commit `f0974e496d0d4b2817ec484dbedb64fdb48b4307`), copied without any `.enola/`.

```sh
enola --generate /fixture/src/cross-repo/cluster.yaml --out /fixture/out/cross-repo
```

| Value | |
| --- | --- |
| `snapshot_id` | `sha256:175d56ebd9c60d648ee400894e51c0d2c3beafd3f6c72c5646ee682f834f4eda` |
| `fact_count` | 20 (20 distinct ids) |
| `facts.jsonl` SHA-256 | `5e230ea16edc9f5e851b78d7e14af463ccd2bd4fc95b8bcba2922bade50a7696` |

Golden tuple (cross-repo call resolved to a handler):

| Role | Fact id | Kind / name | Location |
| --- | --- | --- | --- |
| Client call | `d371930757da8cc5882173de7414d30e` | route `GET /api/v2/orders/{id}`, `role: client` | `web/client.go:10` |
| Server route | `8c598b0d43c049f2e4570f276c7ec3ff` | route `GET /api/v2/orders/{id}` | `api/server.go:27` |
| Handler | `a7c1e9812c4880ea811f1b2e7dab7c09` | symbol `example.com/api.getOrder` | `api/server.go:31` |

Client → server comes from `props.matched_routes[0]` (`id` 8c598b…, `confidence: probable`).
Server → handler is the `handled_by` relation (`target_id` a7c1e9…).
The second resolved call, `GET /api/v2/orders` (`6fbc4f38…` → `95ec3bfa…` → `listOrders` `feed9fd5…`),
has `confidence: verified`.

Deliberately unresolved call: route `/api/v2/{}/orders`, id `c2cbbdeff1bb4e4e0eff329ddd6a806d`,
`web/client.go:21`, `props.unmatched_by_server: true`, `props.unmatched_reason: "path_unknown"`.
It carries no unresolved relation; the marker is the prop. Receipt coverage:
`quality.coverage = {services_total: 2, coverage_gaps: 0, unresolved_edges: 1}`, and the `web`
service fact's `edge_coverage` is `{detected: 3, resolved: 2, unresolved: 1}`.

## orders-api-v0.5.3

Source: the fixture's `api` member as a standalone git repository with
`origin = https://github.com/example-org/orders-api.git` (repo label `orders-api`).
Generated the way `enola-to-graph` will run directory mode, with a private config:

```yaml
# /tmp/run/enola-to-graph.yaml
repo: "/fixture/src/orders-api"
output:
  dir: ".enola"
  root: "/fixture/root"
history:
  enabled: false
```

```sh
enola --generate /tmp/run/enola-to-graph.yaml
```

| Value | |
| --- | --- |
| `snapshot_id` | `sha256:9c303fee5dc65c47bdda3403009d9d6979c1c945b7718ed78ecb0d64a52dae8b` |
| `fact_count` | 10 |
| `facts.jsonl` SHA-256 | `9f9c3184b8c2f00a188f595bdb37af5694a1357381c5bc2ed7787566028496f5` |

Single-repo facts carry `repo: "orders-api"`, unprefixed `file`, and no `repo_prefixed`.

## Determinism

The same commands on macOS arm64, from different source locations, produced
byte-identical `facts.jsonl` and the same `snapshot_id`. Receipts differ only in
`generated_at` and absolute paths, so a drift check compares `facts.jsonl` bytes
and `snapshot_id`, never receipt bytes.
