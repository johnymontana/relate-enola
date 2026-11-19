#!/usr/bin/env bash
# Regenerate the golden Enola fixtures exactly as tests/fixtures/GENERATED.md
# describes, inside an ubuntu:24.04 container with neutral /fixture paths.
#
#   scripts/regen-goldens.sh            # --check (default): regenerate and compare
#   scripts/regen-goldens.sh --write    # regenerate and overwrite the fixtures
#
# --check compares facts.jsonl bytes and snapshot_id, which are deterministic
# across platforms and paths. Receipts are never compared byte for byte
# (generated_at and absolute paths vary).
#
# Requires: bash, curl, git, docker, python3, and shasum or sha256sum.
# Honors TMPDIR for its scratch directory.
set -euo pipefail

mode=${1:---check}
case $mode in --check | --write) ;; *) echo "usage: $0 [--check|--write]" >&2; exit 2 ;; esac

root=$(cd "$(dirname "$0")/.." && pwd)
lock=$root/enola.lock.toml
fixtures=$root/tests/fixtures

# --- pinned Enola, from enola.lock.toml ------------------------------------
top() { sed -n "s/^$1 = \"\(.*\)\".*/\1/p" "$lock" | head -1; }
target_field() { # target_field <target> <key>
	awk -v t="[targets.$1]" -v k="$2" '
		$0 == t { in_t = 1; next }
		/^\[/ { in_t = 0 }
		in_t && $1 == k { gsub(/"/, "", $3); print $3; exit }' "$lock"
}
version=$(top version)
tag=$(top tag)
commit=$(top commit)

case $(uname -m) in
	arm64 | aarch64) arch=arm64 ;;
	x86_64 | amd64) arch=amd64 ;;
	*) echo "unsupported host architecture $(uname -m)" >&2; exit 1 ;;
esac
target=linux-$arch
url=$(target_field "$target" url)
sha=$(target_field "$target" sha256)
binary=$(target_field "$target" binary)
[ -n "$url" ] && [ -n "$sha" ] && [ -n "$binary" ] || { echo "enola.lock.toml has no complete [targets.$target]" >&2; exit 1; }

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

work=$(mktemp -d "${TMPDIR:-/tmp}/enola-goldens.XXXXXX")
trap 'rm -rf "$work"' EXIT
echo "==> Enola $version ($target) into $work"
curl -fsSL --proto '=https' -o "$work/enola.tar.gz" "$url"
actual=$(sha256 "$work/enola.tar.gz")
[ "$actual" = "$sha" ] || { echo "archive checksum mismatch: expected $sha, got $actual" >&2; exit 1; }
mkdir -p "$work/enola" && tar -xzf "$work/enola.tar.gz" -C "$work/enola" "$binary"

# --- sources ---------------------------------------------------------------
echo "==> enola-labs/enola $tag (expect $commit)"
git -c advice.detachedHead=false clone -q --depth 1 --branch "$tag" https://github.com/enola-labs/enola.git "$work/enola-src"
got=$(git -C "$work/enola-src" rev-parse HEAD)
[ "$got" = "$commit" ] || { echo "tag $tag is at $got, expected $commit" >&2; exit 1; }

src=$work/fixture/src
mkdir -p "$src" "$work/fixture/out"
cp -R "$work/enola-src/examples/cross-repo" "$src/cross-repo"
rm -rf "$src"/cross-repo/*/.enola
cp -R "$src/cross-repo/api" "$src/orders-api"
(
	cd "$src/orders-api"
	git init -q
	git add -A
	GIT_AUTHOR_DATE=2026-01-01T00:00:00Z GIT_COMMITTER_DATE=2026-01-01T00:00:00Z \
		git -c user.email=fixture@example.com -c user.name=fixture commit -qm fixture
	git remote add origin https://github.com/example-org/orders-api.git
)

cat >"$work/gen.sh" <<EOF
set -e
export HOME=/tmp/h ENOLA_NO_UPDATE_CHECK=1 ENOLA_NO_PROMPTS=1
mkdir -p "\$HOME"
git config --global --add safe.directory '*'
mkdir -p /tmp/run && cd /tmp/run
E=/enola/$binary
\$E --generate /fixture/src/cross-repo/cluster.yaml --out /fixture/out/cross-repo 2>/fixture/out/cross-repo.stderr
printf 'repo: "/fixture/src/orders-api"\noutput:\n  dir: ".enola"\n  root: "/fixture/root"\nhistory:\n  enabled: false\n' > /tmp/run/enola-to-graph.yaml
\$E --generate /tmp/run/enola-to-graph.yaml 2>/fixture/out/orders-api.stderr
mkdir -p /fixture/out/orders-api
cp /fixture/root/fixture/src/orders-api/.enola/receipt.json /fixture/root/fixture/src/orders-api/.enola/facts.jsonl /fixture/out/orders-api/
EOF

echo "==> generating in ubuntu:24.04 (linux/$arch)"
docker run --rm --platform "linux/$arch" \
	-v "$work/fixture:/fixture" -v "$work/enola:/enola:ro" -v "$work/gen.sh:/gen.sh:ro" \
	ubuntu:24.04 bash -c 'apt-get -qq update >/dev/null && apt-get -qq install -y git >/dev/null 2>&1 && bash /gen.sh'

snapshot_id() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["snapshot_id"])' "$1"; }

status=0
for name in cross-repo orders-api; do
	new=$work/fixture/out/$name
	old=$fixtures/$name-v$version
	if [ "$mode" = --write ]; then
		mkdir -p "$old"
		cp "$new/receipt.json" "$new/facts.jsonl" "$old/"
		echo "wrote $old ($(snapshot_id "$new/receipt.json"))"
		continue
	fi
	if [ ! -d "$old" ]; then
		echo "DRIFT $name: no committed fixture at $old"
		status=1
		continue
	fi
	if cmp -s "$new/facts.jsonl" "$old/facts.jsonl" && [ "$(snapshot_id "$new/receipt.json")" = "$(snapshot_id "$old/receipt.json")" ]; then
		echo "ok    $name: facts.jsonl and snapshot_id match"
	else
		echo "DRIFT $name: regenerated output differs from $old"
		diff <(head -c 4000 "$old/facts.jsonl") <(head -c 4000 "$new/facts.jsonl") | head -20 || true
		status=1
	fi
done
exit $status
