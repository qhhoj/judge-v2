# Changelog

## 2.3.3 (2026-10-14)

### Fixes

- Checkers: testlib `_pc(n)` earns n/200 of the case (Polygon's 0..200 scale), read from testlib's `partially correct (n)` stderr line checked against the exit code. testlib built without `TESTSYS` exits with n itself, so `_pc(0)` was full marks, `_pc(1)`/`_pc(2)` WA/PE and most other n IE; `_pc(0)` is now a wrong answer worth 0.

## 2.3.2 (2026-10-13)

### Fixes

- An archive problem whose data version changes while it is graded is retried, as a loose-file problem already was: its checker, interactor, signature and file I/O sources are read from the data root after resolve, so the result could mix two versions under the old version number.
- Tests: the end-to-end bridged-checker test asserts testlib `points N` as N case points (and N% with `treat_checker_points_as_percentage`), the reading shipped in 2.2.0; it still asserted the pre-2.2.0 fraction reading.
- Checkers: a CRLF line break is one break (rstripped no longer sees an extra empty line per CRLF); Java checkers and interactors get their `-Xmx` filled in (the box memory minus 128 MiB) instead of a literal `{memory_mib}` the JVM rejected; the floats checkers compare `inf`/`nan` and overflowing tokens as text, so identical output matches.
- Daemon: an Assign after a reconnect may take a slot a cancelled generation still holds (it waits for the run to end) instead of failing with SLOTS_FULL; the Final of a run the bridge aborted no longer leaves an outbox that makes the next Hello list more runs than slots.
- Languages: C/C++ compile with `-DONLINE_JUDGE` and Pascal with `-dONLINE_JUDGE`, so `#ifndef ONLINE_JUDGE` freopen templates use stdio; PYPY3 syntax-checks with `py_compile` so a syntax error is CE, not IR; Java main-class detection scans top-level types by brace depth (a braced member before `main` or an inner `public class` no longer misnames the class); a compiler that cannot exec in the box is RETRY RUNTIME_MISSING, not CE.
- Reported memory is the program's peak RSS, not the page cache of its output (an MLE still shows the cgroup peak).
- Interactive problems: the interactor ignores SIGPIPE, so a last write after the solution exited is no longer IE; a solution that fails on EOF or a broken pipe after the interactor quit with WA or points gets the interactor's verdict; an interactor that runs out of its limits, or whose CPU use made the solution time out on the wall clock, is IE instead of the contestant's TLE.

## 2.3.1 (2026-10-12)

### Fixes

- File I/O names match in any letter case, as on the old judge: the input is also linked under each spelling a program may open (usual letter cases plus spellings found in its source), and the output is read from the configured name ignoring case (configured spelling first, else the first match by name).

## 2.3.0 (2026-10-11)

### Features

- Protocol minor 3: a GenerateTests with no tests is a compile check of checker and interactor programs (new roles ROLE_CHECKER, ROLE_INTERACTOR; feature `generate.compile`); it uploads an empty result zip.
- Built-in checkers `rstripped`, `sorted`, `unordered`, `easy` and `linematches` (partial points per line).

## 2.2.0 (2026-10-11)

### Features

- testlib `points N` from a custom checker or interactor is N points of the case, clamped to 0..case points; with `treat_checker_points_as_percentage: true` it is N% of the case points.
- `checker.themis`: bridged checkers with `type: themis` read the test and contestant directories from stdin, find the case files under the problem's file I/O names and the checker source's `.inp`/`.out` literals (every letter case), and print the 0..1 score on the last stdout line.

### Fixes

- Checkers, interactors, generators and validators compile with at least 30 s of CPU (60 s wall): testlib.h alone used most of the 10 s default.
- Release tooling: release PRs carry this note so the change-file check passes.

## 2.1.0 (2026-10-11)

### Features

- Container image ghcr.io/qhhoj/judge-v2 (packaging/Containerfile), built by CI on every push.
- Test generation job (protocol minor 2, feature generate.polygon) and `qhhoj-judge generate`: build a Polygon standard package's tests in isolate.

### Fixes

- Bound memory use: init.yml output_limit_length is capped at 64 MiB, the metrics listener limits and times out connections, the runtime version probe cannot hang on a leaked pipe, and an uploaded output zip is released once written to disk.
- Test suite trimmed to behaviour tests; no runtime change.

## 2.0.1 (2026-10-10)

### Fixes

- PAS, JAVA, JAVA8 and KOTLIN work on Ubuntu 24.04: their runtimes reach `/etc/alternatives` and `/etc/java-21-openjdk` through symlinks, so languages.toml binds exactly those directories read-only into the box.
- Release tarball ships `DEPLOY.md` (judge host guide), `packaging/check-host.sh` and `packaging/nftables-judge.conf` (WireGuard-ready judge firewall).
- BF is no longer in the day-one language set: Ubuntu 24.04 ships no `bf` interpreter (4 legacy submissions used it). Judges report BF as absent; the site disables it at cutover.
- Pushing a `v*` tag publishes a GitHub Release with the tarball and its sha256; DEPLOY.md downloads it with `gh release download`.

## 2.0.0

- Rewrite of the judge as a Rust workspace: `problem` (init.yml, case plan, data version, caches), `sandbox` (ioi/isolate, cgroups v2), `grader` (languages, compile, run, checkers), `daemon` (link, workers, metrics), `cli` (the `qhhoj-judge` binary).
- Talks to the bridge over gRPC (`proto/qhhoj/judge/v1/judge.proto`); the protocol keeps its own `proto-v1.x` tags and compatibility check.
- Test suites: `cargo test --workspace`, plus an isolate suite behind the `isolate` feature.
- DMOJ grading parity: a full run grades `pretest_test_cases` first (a plain pretest worth 0, a pretest batch keeping its points), then `test_cases`; a failed case or batch worth 0 points skips every later case; an interactor that fails (exit 3) or crashes makes the case IE.
