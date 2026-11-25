#!/usr/bin/env bash
# Publishes a signed release to the LynShen software library.
#
#   deploy/software-library/publish.sh <product> <version> <files-dir>
#
# Needs SOFTWARE_SIGNING_KEY (path to the key from `software-release keygen`)
# and SOFTWARE_SSH (user@host of the restricted publisher account). The
# server runs receive-software.py, which checks everything again.
set -euo pipefail
product=${1:?product} version=${2:?version} files=${3:?files dir}
: "${SOFTWARE_SIGNING_KEY:?path to the signing key}" "${SOFTWARE_SSH:?user@host}"
root=$(cd "$(dirname "$0")/../.." && pwd)
out=$(mktemp -d)
stage=$(mktemp -d)
trap 'rm -rf "$out" "$stage"' EXIT
if [ "$product" = cli ]; then
	# Every CLI release also carries the installers from this checkout and a
	# SHA256SUMS of all its files, which the installers and the Homebrew tap
	# check downloads against. The server copies the installers to
	# /cli/install.sh and /cli/install.ps1 (docs/software-library.md).
	cp "$files"/* "$stage"/
	cp "$root/packaging/installer/install.sh" "$root/packaging/installer/install.ps1" "$stage"/
	sums=$(cd "$stage" && shasum -a 256 -- *)
	printf '%s\n' "$sums" >"$stage/SHA256SUMS"
	files=$stage
fi
cargo run -q --release --manifest-path "$root/Cargo.toml" -p lynshen-software-release -- \
	sign "$SOFTWARE_SIGNING_KEY" "$product" "$version" "$files" "$out"
# COPYFILE_DISABLE: macOS tar would add ._* metadata files the server refuses.
COPYFILE_DISABLE=1 tar -czf - -C "$out" . | ssh "$SOFTWARE_SSH" "publish-software $product"
curl -fsS "https://software.lynshen.org/$product/latest.json" >/dev/null
echo "https://software.lynshen.org/$product/latest.json"
