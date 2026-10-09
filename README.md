# judge-v2

## Judge daemon

`qhhoj-judge` grades submissions in ioi/isolate (cgroups v2) and talks to the bridge over gRPC (`proto/qhhoj/judge/v1/judge.proto`).
Crates: `problem` (init.yml, case plan, data version, caches) → `sandbox` (isolate) → `grader` (languages, compile, run, checkers) → `daemon` (link, workers, metrics) → `cli` (the binary).
Tests: `cargo test --workspace`; the isolate suite: `cargo test -p judge-grader --features isolate -- --test-threads=1` (needs isolate 2.x with `isolate.service`).

## Problem-author code (custom checkers, interactors, signature graders)

All of it is compiled and run inside isolate, never on the host, with its own limits: a checker or interactor gets
max(10 s, 2 x TL) CPU and 1 GiB with one process; the interactor's wall is the solution's wall + 2 s. Helpers are
compiled once per content hash (`artefacts`) with `/testlib` mounted read-only. The interactor and the solution are
joined by two pipes whose ends belong to the two isolate children only. A signature problem compiles the
contestant's file with the header, links it with the entry under an unguessable name, and keeps only the binary.

`assets/testlib/testlib.h` is vendored unmodified from MikeMirzayanov/testlib tag 0.9.41 (MIT, see
`assets/testlib/LICENSE`, `VERSION`, `SHA256SUMS`). Only the C++ header is vendored: bridged checkers written in
Pascal or Java that include testlib do not compile (the submission is an internal error).
