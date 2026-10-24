#!/usr/bin/env bash
# packaging/build-release.sh [version]: static binary + data files (selftest programs included), for /opt/qhhoj-judge/releases/<version>.
# The version defaults to the Cargo workspace version (managed by knope); RELEASE_SUFFIX is appended.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
workspace_version="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)"
version="${1:-$workspace_version}${RELEASE_SUFFIX:-}"
rustup target add x86_64-unknown-linux-musl >/dev/null
# the commit goes into Hello.build, so two builds of one version differ
export QHHOJ_JUDGE_REVISION="${QHHOJ_JUDGE_REVISION:-$(git rev-parse HEAD 2>/dev/null || true)}"
cargo build -j2 --release --locked --target x86_64-unknown-linux-musl -p judge-cli
stage="target/release-pkg/qhhoj-judge-$version"
rm -rf "$stage" && mkdir -p "$stage/packaging" "$stage/assets" "$stage/tests"
cp target/x86_64-unknown-linux-musl/release/qhhoj-judge "$stage/"
cp languages.toml README.md LICENSE "$stage/"
cp -r assets/testlib "$stage/assets/"
cp -r tests/programs "$stage/tests/" # the selftest manifest and its programs
cp packaging/config.example.toml packaging/qhhoj-judge.service packaging/check-host.sh packaging/nftables-judge.conf "$stage/packaging/"
cp DEPLOY.md "$stage/"
(cd "$stage" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS)
tar -C target/release-pkg -czf "target/release-pkg/qhhoj-judge-$version.tar.gz" "qhhoj-judge-$version"
echo "target/release-pkg/qhhoj-judge-$version.tar.gz"
