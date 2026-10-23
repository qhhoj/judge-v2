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
An interactive problem is judged by its interactor alone unless `interactive.checker` names a checker (feature
`interactive.checker`): then, once the interactor accepts a case, that checker reads the interactor's `tout` as
the output and decides the case. The top-level and per-case checkers of an interactive problem are never run. A
bridged checker's `files` may be a list: the source first, then headers compiled beside it (a package's own
`testlib.h` among them, found before the vendored one). A checker's or interactor's testlib `points N` is N points
of the case, or N percent of it with `treat_checker_points_as_percentage: true` (a bridged checker's arg, or a key of
`interactive`, feature `interactive.percentage`); N of 0 or less is a wrong answer.

`assets/testlib/testlib.h` is vendored unmodified from MikeMirzayanov/testlib tag 0.9.41 (MIT, see
`assets/testlib/LICENSE`, `VERSION`, `SHA256SUMS`). `assets/testlib/testlib_themis_cms.h`, the testlib 0.9.37
variant with Themis/CMS support that old checkers include, is vendored unmodified from VNOI-Admin/testlib, an MIT
fork of testlib (commit in `VERSION`). Checkers compile without `-DTHEMIS`/`-DCMS`. Only C++ headers are vendored:
bridged checkers written in Pascal or Java that include testlib do not compile (the submission is an internal error).

## Grading outcomes (`judge_submission`)

- Refused before any box is used: an init.yml feature not graded here (generator, `nobigmath`, a custom checker
  of a kind not graded here wherever init.yml names it (problem, interactive, batch or case: `checker.<type>`), a
  non-testlib interactor, per-case checker overrides) is `RETRY UNSUPPORTED`; a language with no runtime on
  this judge (SCRATCH, or a key removed at start) is `RETRY RUNTIME_MISSING`.
- Batches and pretests follow DMOJ: a case that is not AC skips the rest of its batch (SC), `short_circuit` skips
  every later case; a pretest run grades only `pretest_test_cases`. Each case reports `fraction x total`, where a
  batched case carries the batch total; the bridge scores a batch by its weakest case.
- Grading reads from the data root after resolve (a loose-file problem its case files; any problem its checker,
  interactor, signature and file I/O sources), so the upload marker and then `.data-version` are read again at
  the end, for archive problems too: an upload in progress or a moved stamp turns the result into
  `RETRY DATA_UNAVAILABLE`. A compile error is rechecked only on signature problems (their entry and header).
- An OUTPUT submission whose zip cannot be opened (not a zip, corrupt directory) is WA on every case with feedback
  "the submitted zip cannot be read"; an entry with an unsupported method, encryption or a bad checksum makes
  its case WA with the same feedback. These are the contestant's mistakes, not IE. A failed write to the
  judge's scratch file, and a zip with more than 10,000 entries, are IE.
- Compile errors instead of misgrading: a program submitted to an output-only problem, and a TEXT or OUTPUT
  submission to a signature or interactive problem.

## Test generation (Polygon standard packages)

Feature `generate.polygon`, protocol minor 2. The bridge sends `GenerateTests`; the judge fetches the bundle
(`FetchGenerationBundle`, at most 512 MiB), builds every test in its two boxes, uploads a zip of the
`input_name`/`answer_name` entries (`UploadGeneratedTests`, 1 MiB chunks) and sends `GenerationDone` (`ok`, or a
`GenerationFailure` with stage, kind, program, test index, detail). A generation holds one slot like a run, is not
in the per-run outbox, and ends with the session that assigned it (the judge cancels it when the session ends).
`CancelGeneration` and the drain timeout stop it between runs; a cancelled generation ends with `GenerationDone` `STAGE_JOB`/`INTERNAL` "cancelled" (nothing is sent when the session is gone). With no free slot the judge sends `JudgeError` `SLOTS_FULL` and `GenerationDone` "no free slot".

- Bundle: unpacked under the zip limits. Each program is compiled with every file of its bundle directory beside
  it, so `#include "testlib.h"` finds the package's copy (`-I/testlib` is the fallback). An unknown language key
  is `UNSUPPORTED_LANGUAGE`, a compile error `COMPILE_ERROR` with the log.
- Generate: each distinct (generator, args) runs once (argv = args, no stdin, stdout = the input) with the plan's
  generator limits and `max_file_bytes`. Manual inputs are copied with CRLF turned into LF (Polygon's Linux
  package has them that way). A non-zero exit or a signal is `CRASHED` (detail: exit code or signal, then stderr).
- Validate: the validator, if any, reads every input on stdin with `--testset tests --group <group>` (no `--group`
  when the test has none); a non-zero exit is `REJECTED` with its stderr (512 bytes).
- Solve: the one `SOLUTION` program runs on every input (the input and output files when the plan names both),
  with the plan's solution limits plus the JVM reserve.
- Caps: `max_total_bytes` over inputs and answers (`STAGE_JOB`, `OUTPUT_LIMIT`), `max_upload_bytes` over the zip,
  `deadline_s` from receipt (`JOB_TIME_LIMIT`), all checked between runs.

`qhhoj-judge generate --bundle b.zip --plan plan.json --out tests.zip [--languages ... --testlib ...]` runs the same
code locally; the plan is a `GenerateTests` message as proto3 JSON (camelCase). It prints the output path, or the
failure and exits 1. Isolate tests: `crates/grader/tests/generate_isolate.rs` (fixture bundle in
`tests/fixtures/generate`).
