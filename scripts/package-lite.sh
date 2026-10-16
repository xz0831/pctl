#!/usr/bin/env bash
# Build the offline "lite" ZIP: static binary, installer, quick start, smoke test, licenses, checksums.
# Usage: scripts/package-lite.sh [target]   (default x86_64-unknown-linux-musl; needs `rustup target add`)
# Output: dist/pctl-lite-<version>-<target>.zip and .zip.sha256. Refuses to overwrite.
set -euo pipefail
cd "$(dirname "$0")/.."
target=${1:-x86_64-unknown-linux-musl}
ver=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
name=pctl-lite-$ver-$target
zip=$PWD/dist/$name.zip
[ ! -e "$zip" ] || { echo "$zip exists; move it away first" >&2; exit 1; }
# Panic locations embed source paths; keep home and checkout paths out of the binary.
export RUSTFLAGS="--remap-path-prefix=$HOME=/build --remap-path-prefix=$PWD=/pctl"
case $target in
  *-linux-musl) export "CARGO_TARGET_$(tr a-z- A-Z_ <<<"$target")_LINKER=rust-lld" ;; # self-contained, no C toolchain
esac
cargo build --release --locked --target "$target"

stage=$(mktemp -d)/$name
mkdir -p "$stage/bin"
cp "target/$target/release/pctl" "$stage/bin/pctl"
cp LICENSE THIRD_PARTY_NOTICES.txt lite/QUICKSTART.md lite/install.sh lite/pctl.env.example "$stage/"
cp scripts/lite-smoke.sh "$stage/smoke.sh"
chmod 755 "$stage/bin/pctl" "$stage/install.sh" "$stage/smoke.sh"
dirty=$(git status --porcelain --untracked-files=no | grep -q . && echo " (uncommitted changes)" || true)
cat >"$stage/BUILDINFO.txt" <<EOF
pctl lite $ver
target: $target
source: $(git rev-parse HEAD)$dirty
rustc: $(rustc -V)
built: $(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF
if command -v sha256sum >/dev/null; then sum="sha256sum"; else sum="shasum -a 256"; fi
(cd "$stage" && find . -type f ! -name SHA256SUMS | sort | sed 's|^\./||' | xargs $sum >SHA256SUMS)

# Nothing from this computer may ship: home paths, user or host names.
leaks=$({ grep -a -r -l -F -e "$HOME" -e /Users/ "$stage"; grep -a -r -l -w -i -e "$(id -un)" -e "$(hostname -s)" "$stage"; } || true)
[ -z "$leaks" ] || { echo "refusing: local names found in: $leaks" >&2; exit 1; }

mkdir -p dist
(cd "$(dirname "$stage")" && zip -qrX "$zip" "$name")
(cd dist && $sum "$name.zip" >"$name.zip.sha256")
cat "dist/$name.zip.sha256"
