#!/usr/bin/env bash
# Download a Relate release binary for this machine and verify it against the
# release's SHA256SUMS. Prints the path of the installed binary.
#
#   scripts/install-relate.sh [TAG|latest] [DIR]     # default: latest, ./.relate
#
# Requires: gh (authenticated, or GH_TOKEN set), shasum or sha256sum.
set -euo pipefail

tag=${1:-latest}
dir=${2:-.relate}
case "$(uname -s)-$(uname -m)" in
	Linux-x86_64) asset=relate-linux-amd64 ;;
	Linux-aarch64 | Linux-arm64) asset=relate-linux-arm64 ;;
	Darwin-arm64) asset=relate-darwin-arm64 ;;
	Darwin-x86_64) asset=relate-darwin-amd64 ;;
	*) echo "unsupported platform $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
[ "$tag" = latest ] && tag=$(gh release view -R relatedb/relate --json tagName --jq .tagName)
mkdir -p "$dir"
cd "$dir"
gh release download "$tag" -R relatedb/relate -p "$asset" -p SHA256SUMS --clobber
if command -v sha256sum >/dev/null; then
	grep " $asset\$" SHA256SUMS | sha256sum -c - >&2
else
	grep " $asset\$" SHA256SUMS | shasum -a 256 -c - >&2
fi
chmod +x "$asset"
mv "$asset" relate
./relate --version >&2
echo "$PWD/relate"
