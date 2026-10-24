#!/usr/bin/env bash
# packaging/check-release-tag.test.sh: check-release-tag.sh against a stub `gh api` that answers per endpoint.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
bin="$(mktemp -d)"; trap 'rm -rf "$bin"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
# stub: `gh api <path> --jq ...` prints $REF for git/ref/tags/*, $TAG for git/tags/*, $COMPARE for compare/*
cat > "$bin/gh" <<'STUB'
#!/usr/bin/env bash
case "$2" in
  */git/ref/tags/*) echo "$REF" ;;
  */git/tags/*) echo "$TAG" ;;
  */compare/*) echo "$COMPARE" ;;
  *) exit 1 ;;
esac
STUB
chmod +x "$bin/gh"
check() { # tag ref-answer tag-answer compare-answer
  PATH="$bin:$PATH" GITHUB_REPOSITORY=qhhoj/judge-v2 GITHUB_SHA=c0ffee REF="$2" TAG="$3" COMPARE="$4" \
    bash "$here/check-release-tag.sh" "$1" > /dev/null 2>&1
}
check v2.3.9 'tag 7a9' 'true c0ffee' ahead || fail "a signed annotated tag on main publishes"
check v2.3.9 'tag 7a9' 'true c0ffee' identical || fail "a signed tag at the head of main publishes"
check v2.3.9 'commit c0ffee' 'true c0ffee' ahead && fail "a lightweight tag must not publish"
check v2.3.9 'tag 7a9' 'false c0ffee' ahead && fail "an unverified signature must not publish"
check v2.3.9 'tag 7a9' 'true beef' ahead && fail "a tag on another commit than the build must not publish"
check v2.3.9 'tag 7a9' 'true c0ffee' diverged && fail "a tag off main must not publish"
check v2.3.9 'tag 7a9' 'true c0ffee' behind && fail "a commit not yet on main must not publish"
check v2.3.9-rc1 'tag 7a9' 'true c0ffee' ahead && fail "only vX.Y.Z tags publish"
echo "check-release-tag ok"
