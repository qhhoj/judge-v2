---
default: minor
---

`selftest` judges every row of the conformance manifest (`tests/programs/manifest.toml`, now shipped in the release tarball and the image) on the host and exits 1 on any verdict that differs. It runs on boxes 900-901 (refused when they are a live slot's), pinned to the last slot's core at nice 10 (`--box-base`, `--core`, `--nice`); languages the judge does not advertise are skipped unless `--require`d. The manifest's limits and parser move to `judge_grader::manifest`, shared with the isolate suite.
