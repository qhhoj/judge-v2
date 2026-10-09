# Changelog

## 2.0.0

- Rewrite of the judge as a Rust workspace: `problem` (init.yml, case plan, data version, caches), `sandbox` (ioi/isolate, cgroups v2), `grader` (languages, compile, run, checkers), `daemon` (link, workers, metrics), `cli` (the `qhhoj-judge` binary).
- Talks to the bridge over gRPC (`proto/qhhoj/judge/v1/judge.proto`); the protocol keeps its own `proto-v1.x` tags and compatibility check.
- Test suites: `cargo test --workspace`, plus an isolate suite behind the `isolate` feature.
- DMOJ grading parity: a full run grades `pretest_test_cases` first (a plain pretest worth 0, a pretest batch keeping its points), then `test_cases`; a failed case or batch worth 0 points skips every later case; an interactor that fails (exit 3) or crashes makes the case IE.
