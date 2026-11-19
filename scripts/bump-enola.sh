#!/usr/bin/env bash
# Pin a new Enola release (PLAN.md D17). One PR per bump:
#
#   scripts/bump-enola.sh 0.6.0            # rewrite enola.lock.toml, notices, goldens
#   scripts/bump-enola.sh 0.6.0 --print    # print the lock file only; change nothing
#
# For each of the four targets it downloads the release archive, checks it
# against the release's .sha256 file, extracts the binary and records both
# digests. The tag's commit comes from `git ls-remote`, the extractor version
# from the release's version.json. Then it refreshes third_party/enola/
# LICENSE and NOTICE and regenerates the golden fixtures (needs docker).
#
# After a bump: rename the fixture constants in tests (CROSS, ORDERS, their
# snapshot ids), review the facts diff, and update notes/enola-contract.md
# for any contract change. CI's golden-drift job then guards the new pin.
#
# Requires: bash, curl, git, tar, awk, and shasum or sha256sum.
set -euo pipefail

version=${1:?usage: $0 <version> [--print]}
mode=${2:-write}
version=${version#v}
tag=v$version
root=$(cd "$(dirname "$0")/.." && pwd)
base=https://github.com/enola-labs/enola/releases/download/$tag
targets=(darwin-arm64 darwin-amd64 linux-amd64 linux-arm64)

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
fetch() { curl -fsSL --proto '=https' -o "$2" "$1"; }

work=$(mktemp -d "${TMPDIR:-/tmp}/enola-bump.XXXXXX")
trap 'rm -rf "$work"' EXIT

commit=$(git ls-remote https://github.com/enola-labs/enola.git "refs/tags/$tag^{}" "refs/tags/$tag" | awk 'NR==1 {print $1}')
[ -n "$commit" ] || { echo "no tag $tag in enola-labs/enola" >&2; exit 1; }
# An annotated tag lists its peeled commit as ^{}; prefer that line.
peeled=$(git ls-remote https://github.com/enola-labs/enola.git "refs/tags/$tag^{}" | awk '{print $1}')
[ -n "$peeled" ] && commit=$peeled

fetch "$base/version.json" "$work/version.json"
extractor=$(sed -n 's/.*"extractor_version": *"\([^"]*\)".*/\1/p' "$work/version.json")

lock=$work/enola.lock.toml
{
	echo "# Pinned Enola release embedded by the \`bundled-enola\` build (PLAN.md D17)."
	echo "# Update only through scripts/bump-enola.sh, which also regenerates the goldens."
	echo "# Each sha256 was checked on $(date -u +%Y-%m-%d) against the release's .sha256 file and"
	echo "# GitHub's recorded asset digest."
	echo
	echo "version = \"$version\""
	echo "tag = \"$tag\""
	echo "commit = \"$commit\""
	echo "extractor_version = \"$extractor\""
	echo "license = \"Apache-2.0\"   # LICENSE + NOTICE ship inside every archive"
} >"$lock"

for t in "${targets[@]}"; do
	name=enola-$version-$t
	echo "==> $name" >&2
	fetch "$base/$name.tar.gz" "$work/$name.tar.gz"
	fetch "$base/$name.sha256" "$work/$name.sha256"
	want=$(awk '{print $1; exit}' "$work/$name.sha256")
	got=$(sha256 "$work/$name.tar.gz")
	[ "$got" = "$want" ] || { echo "$name.tar.gz: checksum $got does not match $name.sha256 ($want)" >&2; exit 1; }
	mkdir -p "$work/$t"
	tar -xzf "$work/$name.tar.gz" -C "$work/$t" "$name" LICENSE NOTICE
	{
		echo
		echo "[targets.$t]"
		echo "url = \"$base/$name.tar.gz\""
		echo "sha256 = \"$got\""
		echo "binary = \"$name\""
		echo "binary_sha256 = \"$(sha256 "$work/$t/$name")\""
	} >>"$lock"
done

for t in "${targets[@]:1}"; do
	for f in LICENSE NOTICE; do
		cmp -s "$work/${targets[0]}/$f" "$work/$t/$f" || { echo "$f differs between archives (${targets[0]} vs $t)" >&2; exit 1; }
	done
done

if [ "$mode" = --print ]; then
	cat "$lock"
	exit 0
fi

cp "$lock" "$root/enola.lock.toml"
for f in LICENSE NOTICE; do
	if ! cmp -s "$work/${targets[0]}/$f" "$root/third_party/enola/$f"; then
		echo "==> third_party/enola/$f changed; review the diff" >&2
		cp "$work/${targets[0]}/$f" "$root/third_party/enola/$f"
	fi
done
echo "==> regenerating golden fixtures for $version" >&2
"$root/scripts/regen-goldens.sh" --write
echo "Pinned Enola $version. Next: update the fixture constants in the tests, review diffs, run CI." >&2
