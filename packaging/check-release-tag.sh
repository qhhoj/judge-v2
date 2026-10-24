#!/usr/bin/env bash
# packaging/check-release-tag.sh <tag>: a release (GitHub Release, :vX.Y.Z image) publishes only from a vX.Y.Z tag that
# is annotated, whose signature GitHub verified (the tagger's registered signing key), that points at the commit this
# run built, and whose commit is on main. Reads GitHub through `gh api` (GH_TOKEN, GITHUB_REPOSITORY, GITHUB_SHA).
set -euo pipefail
die() { echo "check-release-tag: $*" >&2; exit 1; }
tag="${1:?usage: check-release-tag.sh <tag>}"
repo="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY}"
[[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "$tag is not a vX.Y.Z tag"
read -r type object <<< "$(gh api "repos/$repo/git/ref/tags/$tag" --jq '.object.type + " " + .object.sha')"
[ "$type" = tag ] || die "$tag is a lightweight tag: release tags are annotated and signed (git tag -s)"
read -r verified commit <<< "$(gh api "repos/$repo/git/tags/$object" --jq '"\(.verification.verified) \(.object.sha)"')"
[ "$verified" = true ] || die "GitHub did not verify the signature of $tag"
[ "$commit" = "${GITHUB_SHA:?GITHUB_SHA}" ] || die "$tag points at $commit, this run built $GITHUB_SHA"
status="$(gh api "repos/$repo/compare/$commit...main" --jq .status)"
case "$status" in
  identical | ahead) echo "check-release-tag: $tag is signed and on main" ;;
  *) die "$tag ($commit) is not on main (compare: $status)" ;;
esac
