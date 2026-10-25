# Changelog

## 2.4.1 (2026-10-25)

### Fixes

- The standard checker splits tokens on vertical tab as legacy did, and the daemon refuses assignments with a zero, negative, non-finite or oversized limit.
- The release workflow refreshes Cargo.lock again.
- Release 2.4.0 carries this note so the change-file check passes.

## 2.4.0 (2026-10-24)

### Features

- The daemon refuses to start when nothing bounds what runs write to isolate's box root: `[isolate] disk_quota` must be set and settable, or the box root must be a filesystem of its own (`allow_unbounded_disk` for loopback dev stacks only). A configured quota isolate cannot set is now a start error, not a warning. JAVA, JAVA8 and KOTLIN pin `-XX:MaxRAM` (javac too), so the JVM heap no longer depends on the host's RAM.
- Report the CPU of the custom checker and the interactor per case as `Case.jury_time_s` (additive proto field, protocol minor 4). Built-in checkers report 0; verdicts, scores and limits are unchanged.
- `selftest` judges every row of the conformance manifest (`tests/programs/manifest.toml`, now shipped in the release tarball and the image) on the host and exits 1 when any case's status differs. It runs on boxes 900-901 pinned to a core no slot owns, at nice 10 (`--box-base`, `--core`, `--nice`), and refuses a slot's box or core; languages the judge does not advertise are skipped unless `--require`d. Manifest rows may give per-case statuses (`cases`), unknown keys are refused, and the limits and parser move to `judge_grader::manifest`, shared with the isolate suite, which now also checks every case.

### Fixes

- Agent skill configuration (issue tracker, triage labels, domain docs) in CLAUDE.md and docs/agents/; docs only.
- Hello.build is the version plus the commit (`2.3.9+<12 hex>`) when the build knows it (the image's `REVISION`, `build-release.sh`), else the version alone, so the bridge can tell two builds of one release apart.
- Internal: the sandbox can start a manager and a contestant joined by a sealed shared-memory channel on fds 3-5 (isolate `--inherit-fds`), refusing to start while the daemon holds an inheritable fd. Not used by grading yet.
- `qhhoj-judge check-config` runs the daemon's config, pinning and disk-bound checks (the quota probed on a spare box) and exits non-zero when one fails, so an install can check a config before switching to a release.
- Internal: `Reporter::cases` reports several cases as one arrival for the daemon's batcher (default: one `case` call each). Only the conformance replay uses it, so a transcript's Cases message is no longer split by the 100 ms batch age on a starved host. Grading is unchanged.
- Internal: a conformance step's 5 s wait no longer counts time the host did not run the test process (at most 60 s more), so a starved CI host no longer fails a scenario. No runtime change.
- Internal: the data-version wait tests move the stamp and the upload marker between two polls through a scripted stamp source instead of a sleeping thread, so a starved host no longer fails them. No runtime change.
- Point to the shared domain glossary; time out a hung apt step in the isolate CI job.
- Internal: new `judge-interface` crate (split signature): header grammar v1 with line/column refusals, roles from compiler symbol tables, the interface log and the generated C++ glue. Not used by grading yet.
- Docs for the protocol, grading, operations and architecture; comments cite repo paths instead of unreachable documents; a CI grep keeps it so.
- Test that a JVM in a box pinned to one core sees one processor, so JAVA, JAVA8 and KOTLIN size their JIT threads the same on every host; `languages.toml` notes why no `-XX:ActiveProcessorCount` is needed.
- A case, batch or pretest checker of a kind this judge does not grade (bridged `type: cms`, ...) is refused at init as `RETRY UNSUPPORTED` (`checker.<type>`), as a problem-wide one already was, instead of an internal error while grading.
- PyPy 3 compiles and runs pin `PYPY_GC_NURSERY=4M` and `PYPY_GC_MAX_DELTA=2GB`, so a PyPy verdict near the memory limit no longer depends on the judge host's CPU cache (PyPy sized its nursery from 1 MiB to 130 MiB) or RAM. Compile runs now get the language's `env` too.
- Releases publish only from a signed tag on main and carry build provenance attestations (tarball and image); the image build pins protoc and rustup-init by checksum.
- Protocol contract: RetryReason documents how the bridge treats each reason. Conformance 27 pins the RETRY cap on UNSPECIFIED (a failure). The new 27b pins that transient reasons (DATA_UNAVAILABLE, RUNTIME_MISSING, SANDBOX_FAILURE) never spend it, 27c that they end as IE once the bridge's deferral window has passed, and 27d that a transient RETRY between failures neither spends nor resets the cap. No scenario depends on how fast a run cycle is. The protocol and decision docs state the new rule. The judge sends the same messages as before.

## 2.3.9 (2026-10-23)

### Fixes

- Every isolate run removes its box's previous meta file before it starts, and a run whose meta file is missing, empty or has neither `time` nor `status` is a sandbox failure (RETRY). Before, an interactive run whose isolate was killed before writing its meta file was graded from the previous run's meta, and an empty meta file read as a clean exit.

## 2.3.8 (2026-10-23)

### Fixes

- JAVA, JAVA8 and KOTLIN bind the `/etc/java-NN-openjdk` directory of the JDK `/usr/lib/jvm/default-java` links to (`{jdk_etc}` in languages.toml) instead of a hard-coded JDK 21 path, so they work on hosts with another JDK major; a language whose bound directory is missing is not advertised, instead of retrying every job as a sandbox failure.
- Internal: interactive grading runs on a new `session` module (spawn, wait, verdict predicates, testlib score, jury file save) that split signature and Communication will share. No verdict, time or message changes.
- init.yml `signature_grader` accepts `split` and `cores`; `split` with `allow_main`, `cores` without `split` or outside 1..2, and `split` together with `interactive`, `output_only` or `file_io` are refused. Until split signature is graded, a split problem answers `RETRY UNSUPPORTED` (`signature.split`, `signature.split.paired`) instead of an internal error. Legacy files parse exactly as before.

## 2.3.7 (2026-10-23)

### Fixes

- Boxes can be pinned by isolate cpuset lines (`qhhoj-judge isolate-cpus`) and given a per-box disk quota (`[isolate] disk_quota`, a headroom over the files each box is given and the output its run may write); both are off until the host is set up (DEPLOY.md), with a warning at start.
- CI installs protoc 29.3 by a direct, sha256-pinned release download instead of arduino/setup-protoc, whose GitHub API lookup failed jobs intermittently. No runtime change.
- A Java source with a `package` line, or with a class name too long for its `<class>.java` file name (over 255 bytes), is a compile error with a message saying why, instead of a run-time failure on every case or a sandbox-failure retry ending in IE.
- Memory limit: the program's own memory is the cgroup peak less the page cache of its own input, output and kept stderr (output files counted by allocated blocks), never under the peak RSS and never over the cgroup peak. Before this it was the smaller of the two counters, so 60 MiB of heap plus 22 MiB written to `/dev/shm` (or a memfd) and read back unmapped showed 62 MiB and passed a 64 MiB limit. A timed-out run, and compilers and jury programs, keep the smaller counter. Runs no longer get a writable `/dev/shm`.
- An OUTPUT zip whose relayouts all find the same entries (`out/1.a` for case `tests/out/1.a`, or one nested case) is lined up instead of treated as a tie and read as empty.
- Checkers and interactors: testlib `points N` (and a default checker's exit 2 share) with N of 0 or less is a wrong answer. It was AC with 0 points: counted as accepted, and it did not stop a batch, a short-circuit run, or a full run at a failed pretest. An interactor's `points N` is N percent of the case when `interactive` sets `treat_checker_points_as_percentage: true` (optional, default false; feature `interactive.percentage`); before, it was always N points of the case.

## 2.3.6 (2026-10-21)

### Fixes

- An interactive problem's `interactive.checker` runs on the interactor's `tout` once the interactor accepts; a bridged checker's `files` may list headers (and the package's testlib.h) compiled beside it.

## 2.3.5 (2026-10-18)

### Fixes

- `check-host.sh` fails a host whose isolate cgroup has no `memory.peak` (kernel before 5.19). There isolate reports no cgroup memory, and the memory limit is held to the peak RSS alone, which counts a runtime's shared objects as the program's.
- Tests: an isolate test runs a real testlib interactor that traces to stderr before `quitp` and `_pc(n)`, and checks its verdict is still read.
- Memory limit: the cgroup gets room above the memory limit for the page cache of the files a run writes (one `--fsize` worth, at most 64 MiB), and MLE is the program's own memory over the limit, or an OOM kill at the raised cgroup limit. The program's own memory is the smaller of the cgroup peak, which also counts the page cache of written output, and the peak RSS, which also counts mapped files already in page cache (PyPy's ~37 MiB libpypy), or the one isolate reported when it reported only one. A run killed at the time limit is not MLE unless the cgroup OOM-killed it: on a timeout isolate reports its own proxy's peak RSS. Before this, the page cache of a large output counted against a small memory limit, so a run could be OOM-killed and shown as MLE while the program itself used 2 MiB. Reported memory is the program's own; an MLE shows the larger of the cgroup peak and the peak RSS, so it is never shown under the limit.
- Output limit: a run whose output went over the limit is OLE even when it then ran out of time. The JVM ignores SIGXFSZ and `System.out` and `PrintWriter` swallow the write error, so a Java or Kotlin program printing in a loop ran on after its output was cut at the limit and was shown as TLE. A run is judged MLE first, then OLE, then TLE.
- Output-only: an OUTPUT zip whose outer folders were dropped, added or renamed against the case `out` names is lined up once per submission, so its files are found instead of every case getting an empty output (WA). A layout that would give two cases one file is never used.
- Checkers: a testlib checker that prints debug lines to stderr before quitting is read from its verdict line (`points N`, `partially correct (n)`, `ok`, `wrong answer`, ...): `quitp` after debug output was IE, or took a number from the debug text as the score.
- Themis checkers: the two directories on the checker's stdin end with `/`, as the old judge gave them, so `dir + "name.inp"` without a separator opens the right file. Each case's files are also laid out under the case's own input and answer file names, in every letter case and with any extension, and `.ans` literals in the checker source name the answer and output. Before this, a checker that names its files `name ".INP"` with no file I/O set was IE on every submission, and `dir + "polygon.inp"` or a `.ans` answer found no files. Extra names are hard links, not copies.

## 2.3.4 (2026-10-15)

### Fixes

- Vendor testlib_themis_cms.h (VNOI-Admin/testlib, MIT) next to testlib.h so checkers that include it compile.

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
