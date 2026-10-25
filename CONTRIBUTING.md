# Contributing

## Versioning and changelog

The judge follows Semantic Versioning (`[workspace.package] version` in `Cargo.toml`, starting at 2.0.0; every crate uses `version.workspace = true`): major = breaking protocol or on-disk/DB format change, minor = feature, patch = fix. Tooling is [knope](https://knope.tech) 0.23.0 (`cargo install knope --version 0.23.0 --locked`), which reads Changesets-format notes (conventional commits are ignored) from `.changeset/*.md`.

- Every PR adds a change file: `knope document-change` (pick the bump, write one line) and commit it. CI fails a pull request that adds no `.changeset/*.md`; pushes to `main` are not checked.
- Cutting a release: `knope release` (bumps the workspace version, writes `CHANGELOG.md`, deletes the consumed change files, refreshes `Cargo.lock`, commits), then `knope tag` (creates the `v<version>` git tag locally). The tag must be annotated and signed with a key registered on your GitHub account (`git config tag.gpgSign true`, or tag by hand with `git tag -s`) and point at the release commit on main: the release workflow refuses any other tag (`packaging/check-release-tag.sh`).
- The gRPC protocol is versioned separately: it keeps its own `proto-v1.x` tags, which CI reads for the shape compatibility check. A judge release does not create or move them.

## Isolate tests

The isolate suites (`--features isolate`) need isolate 2.x with `isolate.service`, and share isolate boxes, so run one suite at a time. To serialise runs on a shared host, take a lock with `flock -o`:

```sh
flock -o /path/to/isolate.lock cargo test --locked -p judge-sandbox -p judge-grader --features isolate -- --test-threads=1
```

`-o` closes the lock fd before the tests start. Channel runs (`--inherit-fds`) refuse to start while the process holds any fd that is not close-on-exec, and `flock` without `-o` passes its lock fd on. The same holds for any wrapper that starts the tests or the daemon: it must pass only fds 0-2. CI starts its test steps through `.github/scripts/only-stdio.sh`, which closes the inheritable fds the GitHub runner passes to each step.

## Backend contract fixtures

`crates/problem/tests/fixtures/judge-contract` is a copy of backend-v2's `libs/problems/src/judge-contract/fixtures`, written by the backend's real init.yml writer, archive reader and `ZIP_LIMITS`. `judge_contract_init_yml` (grader) and `judge_contract_zips` (problem) check that the judge reads each file as the backend meant, and that `BACKEND_ARCHIVE_MAX_BYTES` matches the backend's cap. Never edit the copy: refresh it from a backend-v2 checkout whose fixtures are committed, `rm -rf crates/problem/tests/fixtures/judge-contract && cp -r ../backend-v2/libs/problems/src/judge-contract/fixtures crates/problem/tests/fixtures/judge-contract`, and commit. A failure after a refresh is a real mismatch: fix the side that is wrong, not the fixture.
