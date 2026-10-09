# Contributing

## Versioning and changelog

The judge follows Semantic Versioning (`[workspace.package] version` in `Cargo.toml`, starting at 2.0.0; every crate uses `version.workspace = true`): major = breaking protocol or on-disk/DB format change, minor = feature, patch = fix. Tooling is [knope](https://knope.tech) 0.23.0 (`cargo install knope --version 0.23.0 --locked`), which reads Changesets-format notes (conventional commits are ignored) from `.changeset/*.md`.

- Every PR adds a change file: `knope document-change` (pick the bump, write one line) and commit it. CI fails a pull request that adds no `.changeset/*.md`; pushes to `main` are not checked.
- Cutting a release: `knope release` (bumps the workspace version, writes `CHANGELOG.md`, deletes the consumed change files, refreshes `Cargo.lock`, commits), then `knope tag` (creates the `v<version>` git tag locally).
- The gRPC protocol is versioned separately: it keeps its own `proto-v1.x` tags, which CI reads for the shape compatibility check. A judge release does not create or move them.
