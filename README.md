# judge-v2

## Judge daemon

`qhhoj-judge` grades submissions in ioi/isolate (cgroups v2) and talks to the bridge over gRPC (`proto/qhhoj/judge/v1/judge.proto`).
Crates: `problem` (init.yml, case plan, data version, caches) → `sandbox` (isolate) → `grader` (languages, compile, run, checkers) → `daemon` (link, workers, metrics) → `cli` (the binary).
Tests: `cargo test --workspace`; the isolate suite: `cargo test -p judge-grader --features isolate -- --test-threads=1` (needs isolate 2.x with `isolate.service`).
