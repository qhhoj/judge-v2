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

## Grading outcomes (`judge_submission`)

- Refused before any box is used: an init.yml feature not graded here (generator, `nobigmath`, a non-testlib
  custom checker or interactor, per-case checker overrides) is `RETRY UNSUPPORTED`; a language with no runtime on
  this judge (SCRATCH, or a key removed at start) is `RETRY RUNTIME_MISSING`.
- Batches and pretests follow DMOJ: a case that is not AC skips the rest of its batch (SC), `short_circuit` skips
  every later case; a pretest run grades only `pretest_test_cases`. Each case reports `fraction x total`, where a
  batched case carries the batch total; the bridge scores a batch by its weakest case.
- A loose-file problem (no archive) reads its case files from the data root while grading, so the upload marker
  and then `.data-version` are read again at the end: an upload in progress or a moved stamp turns the result into
  `RETRY DATA_UNAVAILABLE`. A compile error is rechecked only on signature problems (their entry and header).
- An OUTPUT submission whose zip cannot be opened (not a zip, corrupt directory) is WA on every case with feedback
  "the submitted zip cannot be read"; an entry with an unsupported method, encryption or a bad checksum makes
  its case WA with the same feedback. These are the contestant's mistakes, not IE. A failed write to the
  judge's scratch file, and a zip with more than 10,000 entries, are IE.
- Compile errors instead of misgrading: a program submitted to an output-only problem, and a TEXT or OUTPUT
  submission to a signature or interactive problem.
