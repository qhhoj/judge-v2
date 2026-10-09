#!/usr/bin/env bash
# packaging/build-release.sh <version>: static binary + data files, for /opt/qhhoj-judge/releases/<version>.
set -euo pipefail
version="${1:?usage: build-release.sh <version>}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
rustup target add x86_64-unknown-linux-musl >/dev/null
cargo build -j2 --release --locked --target x86_64-unknown-linux-musl -p judge-cli
stage="target/release-pkg/qhhoj-judge-$version"
rm -rf "$stage" && mkdir -p "$stage/packaging" "$stage/assets"
cp target/x86_64-unknown-linux-musl/release/qhhoj-judge "$stage/"
cp languages.toml README.md "$stage/"
cp -r assets/testlib "$stage/assets/"
cp packaging/config.example.toml packaging/qhhoj-judge.service "$stage/packaging/"
(cd "$stage" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS)
tar -C target/release-pkg -czf "target/release-pkg/qhhoj-judge-$version.tar.gz" "qhhoj-judge-$version"
echo "target/release-pkg/qhhoj-judge-$version.tar.gz"
