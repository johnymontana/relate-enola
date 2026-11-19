#!/usr/bin/env bash
# End-to-end smoke test of an enola-to-graph package against a real relate
# (PLAN.md Phase 4 release gates). Runs in an isolated HOME and config dir with
# Enola absent from PATH.
#
#   RELATE_BIN=... scripts/smoke.sh --from <package-dir>
#   RELATE_BIN=... scripts/smoke.sh --github <owner/repo@tag>
#
# Install happens first (it may use the network). Set SMOKE_OFFLINE="sudo unshare --net --"
# on Linux to run every step after the install without network access.
# For a private repository, set GITHUB_TOKEN for the --github install.
#
# Checks: extension check (for --from), install, live directory-mode and cluster
# generation reproducing the golden snapshot ids, an untouched checkout and
# ~/.enola, an unchanged re-run, and a --snapshot import of the golden fixture.
set -euo pipefail

mode=${1:?usage: smoke.sh --from <dir> | --github <owner/repo@tag>}
what=${2:?usage: smoke.sh --from <dir> | --github <owner/repo@tag>}
relate=${RELATE_BIN:?set RELATE_BIN}
root=$(cd "$(dirname "$0")/.." && pwd)
offline=${SMOKE_OFFLINE:-}
CROSS_ID=sha256:175d56ebd9c60d648ee400894e51c0d2c3beafd3f6c72c5646ee682f834f4eda
ORDERS_ID=sha256:9c303fee5dc65c47bdda3403009d9d6979c1c945b7718ed78ecb0d64a52dae8b

work=$(mktemp -d "${TMPDIR:-/tmp}/enola-smoke.XXXXXX")
mkdir -p "$work/home" "$work/cfg" "$work/src"
fail() { echo "SMOKE FAIL: $*" >&2; exit 1; }
step() { echo "==> $*" >&2; }

# relate with the isolated environment; online for install, $offline afterwards.
R_online() { env -i PATH=/usr/bin:/bin HOME="$work/home" XDG_CONFIG_HOME="$work/cfg" DO_NOT_TRACK=1 \
	${GITHUB_TOKEN:+GITHUB_TOKEN="$GITHUB_TOKEN"} "$relate" "$@"; }
# safe.directory=* via the environment reaches Enola's own git calls too, which
# matters when $offline runs them as root against a checkout owned by the runner.
R() { $offline env -i PATH=/usr/bin:/bin HOME="$work/home" XDG_CONFIG_HOME="$work/cfg" DO_NOT_TRACK=1 \
	GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0='*' "$relate" "$@"; }
G() { $offline git -c safe.directory='*' "$@"; }
latest_ids() { R query "MATCH (:EnolaSource)-[:ENOLA_LATEST]->(s) RETURN s.snapshot_id AS id ORDER BY id" \
	--database "$1" --mode jsonl | sed -n 's/.*"id":"\([^"]*\)".*/\1/p' | tr '\n' ' '; }

case $mode in
--from)
	step "extension check $what"
	R_online extension check "$what" >&2
	[ ! -e "$work/home/.enola" ] || fail "extension check started Enola"
	step "install --from"
	R_online extension install enola-to-graph --from "$what" >&2
	;;
--github)
	step "install --github $what"
	R_online extension install enola-to-graph --github "$what" >&2
	R_online extension list --trust >&2
	R_online extension list --trust | grep -q enola-to-graph || fail "no trust-store pin recorded"
	;;
*) fail "unknown mode $mode" ;;
esac
command -v enola >/dev/null 2>&1 && echo "note: an enola is on this machine's PATH; the isolated PATH excludes it" >&2

# Fixture sources: the cross-repo cluster and orders-api as its own git repo.
cp -R "$root/tests/fixtures/src/cross-repo" "$work/src/cross-repo"
cp -R "$root/tests/fixtures/src/cross-repo/api" "$work/src/orders-api"
(
	cd "$work/src/orders-api"
	git init -q && git add -A
	git -c user.email=fixture@example.com -c user.name=fixture commit -qm fixture
	git remote add origin https://github.com/example-org/orders-api.git
)
[ -z "$offline" ] || step "running the remaining steps with: $offline"

step "directory mode"
out=$(R run enola-to-graph "$work/src/orders-api" -d "$work/dir.rl" 2>&1) || fail "directory mode: $out"
echo "$out" | head -c 400 >&2; echo >&2
[ "$(latest_ids "$work/dir.rl")" = "$ORDERS_ID " ] || fail "directory mode snapshot id: $(latest_ids "$work/dir.rl")"
[ -z "$(G -C "$work/src/orders-api" status --porcelain --ignored)" ] || fail "the checkout was modified"
[ ! -e "$work/home/.enola" ] || fail "~/.enola was created"

step "unchanged re-run"
out=$(R run enola-to-graph "$work/src/orders-api" -d "$work/dir.rl" 2>&1) || fail "re-run: $out"
case $out in *"found nothing changed"*"already imported"*) ;; *) fail "re-run was not a no-op: $out" ;; esac

step "cluster mode"
out=$(R run enola-to-graph "$work/src/cross-repo/cluster.yaml" -d "$work/cluster.rl" 2>&1) || fail "cluster mode: $out"
[ "$(latest_ids "$work/cluster.rl")" = "$CROSS_ID " ] || fail "cluster snapshot id: $(latest_ids "$work/cluster.rl")"

step "--snapshot import of the golden fixture"
out=$(R run enola-to-graph "$root/tests/fixtures/cross-repo-v0.5.3" --snapshot -d "$work/golden.rl" 2>&1) || fail "--snapshot: $out"
[ "$(latest_ids "$work/golden.rl")" = "$CROSS_ID " ] || fail "golden snapshot id"
calls=$(R query "MATCH (c:EnolaFact)-[m:ENOLA_MATCHES_ROUTE]->(:EnolaFact)-[:ENOLA_HANDLED_BY]->(h:EnolaFact) RETURN h.name AS handler, m.confidence AS confidence ORDER BY handler" --database "$work/golden.rl" --mode jsonl)
echo "$calls" | grep -q '"confidence":"probable","handler":"example.com/api.getOrder"\|"handler":"example.com/api.getOrder","confidence":"probable"' ||
	fail "golden tuple missing: $calls"

echo "SMOKE PASS ($mode $what${offline:+, offline after install})" >&2
