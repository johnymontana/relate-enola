#!/usr/bin/env bash
# Download and verify the pinned Enola binary for one target, for a
# `bundled-enola` build (PLAN.md Phase 3; the release workflow uses it too).
#
#   scripts/fetch-enola.sh [TARGET] [OUT_DIR]
#
# TARGET is an enola.lock.toml key: darwin-arm64, darwin-amd64, linux-amd64
# or linux-arm64 (default: this machine). OUT_DIR defaults to
# target/enola/<TARGET>. The script verifies the archive SHA-256, extracts only
# the binary plus LICENSE/NOTICE, verifies the binary SHA-256, checks the
# notices match third_party/enola/, and prints the ENOLA_BIN line to use:
#
#   export $(scripts/fetch-enola.sh | tail -1)
#   cargo build --release --features bundled-enola
#
# Requires: bash, curl, tar, and shasum or sha256sum.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
lock=$root/enola.lock.toml

host_target() {
	local os arch
	case $(uname -s) in Darwin) os=darwin ;; Linux) os=linux ;; *) echo "unsupported OS $(uname -s)" >&2; exit 1 ;; esac
	case $(uname -m) in arm64 | aarch64) arch=arm64 ;; x86_64 | amd64) arch=amd64 ;; *) echo "unsupported arch $(uname -m)" >&2; exit 1 ;; esac
	echo "$os-$arch"
}

target=${1:-$(host_target)}
out=${2:-$root/target/enola/$target}

field() { # field <key>
	awk -v t="[targets.$target]" -v k="$1" '
		$0 == t { in_t = 1; next }
		/^\[/ { in_t = 0 }
		in_t && $1 == k { gsub(/"/, "", $3); print $3; exit }' "$lock"
}
sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

url=$(field url)
archive_sha=$(field sha256)
binary=$(field binary)
binary_sha=$(field binary_sha256)
[ -n "$url" ] && [ -n "$archive_sha" ] && [ -n "$binary" ] && [ -n "$binary_sha" ] ||
	{ echo "enola.lock.toml has no complete [targets.$target]" >&2; exit 1; }

mkdir -p "$out"
archive=$out/$(basename "$url")
if [ ! -f "$archive" ] || [ "$(sha256 "$archive")" != "$archive_sha" ]; then
	echo "downloading $url" >&2
	curl -fsSL --proto '=https' -o "$archive.part" "$url"
	mv "$archive.part" "$archive"
fi
got=$(sha256 "$archive")
[ "$got" = "$archive_sha" ] || { echo "archive checksum mismatch: expected $archive_sha, got $got" >&2; exit 1; }

tar -xzf "$archive" -C "$out" "$binary" LICENSE NOTICE
got=$(sha256 "$out/$binary")
[ "$got" = "$binary_sha" ] || { echo "binary checksum mismatch: expected $binary_sha, got $got" >&2; exit 1; }
for f in LICENSE NOTICE; do
	cmp -s "$out/$f" "$root/third_party/enola/$f" ||
		{ echo "the archive's $f differs from third_party/enola/$f; update the vendored notices" >&2; exit 1; }
done
chmod +x "$out/$binary"
echo "verified Enola $(sed -n 's/^version = "\(.*\)"/\1/p' "$lock") for $target" >&2
echo "ENOLA_BIN=$out/$binary"
