---
default: minor
---

`selftest` judges every row of the conformance manifest (`tests/programs/manifest.toml`, now shipped in the release tarball and the image) on the host and exits 1 when any case's status differs. It runs on boxes 900-901 pinned to a core no slot owns, at nice 10 (`--box-base`, `--core`, `--nice`), and refuses a slot's box or core; languages the judge does not advertise are skipped unless `--require`d. Manifest rows may give per-case statuses (`cases`), unknown keys are refused, and the limits and parser move to `judge_grader::manifest`, shared with the isolate suite, which now also checks every case.
