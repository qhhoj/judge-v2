# Architecture

`qhhoj-judge` is one daemon that holds a gRPC stream to the bridge, runs several submissions in parallel (one per slot),
and grades each in ioi/isolate boxes. Words follow the shared [GLOSSARY](../GLOSSARY.md); its "Judge and Bridge
internals" section defines Assignment, Case, Box, Jury and the rest.

## Crates

Naming rule: `crates/<x>` is package `judge-<x>`. Two exceptions: `judge-proto` (its directory already has the prefix)
and `conformance` (test support, no prefix). The binary is `qhhoj-judge`, built by `judge-cli`.

| Crate | Role |
|---|---|
| `problem` | init.yml parser (strict subset), case plan (pretests, batches, points), verdict codes, data version, problem resolve and zip extraction |
| `sandbox` | isolate wrapper: init, run, cleanup, meta parsing, verdict classification, box ids |
| `grader` | languages, compile, run one case, built-in and bridged checkers, interactor, signature, output-only, test generation |
| `daemon` | config, gRPC link, worker pool, outbox, batcher, caches, metrics; the library behind the binary |
| `cli` | package `judge-cli`, the `qhhoj-judge` binary: the daemon (default), `run`, `generate`, `selftest`, `check-config`, `isolate-cpus` |
| `judge-proto` | tonic and prost types generated from `proto/` at build time (nothing generated is checked in) |
| `conformance` | the scenario player and scripted fake bridge that pin the protocol (`proto/conformance`) |
| `interface` | split-signature header grammar, roles, generated C++ glue and the RPC runtime (`runtime/`) |

`grader` uses `problem` and `sandbox`, `daemon` uses `grader`, and `cli` uses `daemon`; `judge-proto` is used by `daemon` and
`conformance`.

## One Judging, from the bridge message to the reported Verdict

1. `cli/src/main.rs` starts `daemon/src/run.rs`, which wires config, logs, languages, boxes, workers, metrics and the link.
2. `daemon/src/link.rs` owns the gRPC stream: `Hello`/`Welcome`, resume, and `handle` for each bridge message.
   `wire.rs` converts proto messages to and from grader types.
3. `daemon/src/pool.rs` `assign` takes a free slot, opens the assignment's outbox (`outbox.rs`) and hands the job to that
   slot's worker.
4. `daemon/src/worker.rs` `process` runs the lifecycle: `Accepted`, fetch the source (`source.rs`), resolve the problem
   (`problem/src/resolve.rs`), grade, flush the last cases, `Final`.
5. `grader/src/judge.rs` `judge_submission` plans the cases, refuses unsupported features, compiles (`compile.rs`),
   chooses the checker (`checkers/select.rs`) and calls `grade.rs`.
6. `grade.rs` `grade_cases` walks the case plan (short-circuit, batches, points). For each case `case_judges.rs` runs
   `run_case.rs`, which puts the input in the solution box, calls `sandbox` and turns the meta file into a run verdict
   (`sandbox/src/classify.rs`), then asks the checker for AC, WA or partial points. `rerun.rs` judges a near-miss TLE
   case again with the host alone; every isolate run holds the run gate (`sandbox/src/run_gate.rs`).
7. Each case becomes a `CaseReport` (`report.rs`) sent through the `Reporter` to the worker, which batches them
   (`batcher.rs`: every 100 ms or 64 cases) into outbox messages.
8. `link.rs` `flush` sends the outbox messages in `seq` order; `ResultAck` trims them. `Final` is last.

To follow the same path with scripted messages instead of code, read scenario `11-grade` first
([`proto/README.md`](../proto/README.md)).

## Boxes

Worker *i* owns two isolate boxes, `box_base + 2i` and `box_base + 2i + 1` (`sandbox/src/boxes.rs`):

- the **solution box** runs the contestant's compile and every case;
- the **jury box** (the code calls it the helper box) runs problem-author programs: a bridged checker, the interactor,
  a generator, a signature entry.

No two workers share a box, and the daemon cleans every box it owns at start and after each assignment.

## Verdict rules

Summarised here; the full rules (order MLE, OLE, TLE, memory counters, JVM reserves) are in
[grading.md](grading.md#verdict-rules).

- A run is classified by isolate status, memory counters and output size; the checker only sees runs that ended normally.
- A case that is not AC skips the rest of its batch; `short_circuit` skips every later case; a failed case or batch worth
  0 points stops the run even without it.
- Contestant mistakes (an unreadable OUTPUT zip, a compile error) are WA or CE. Judge faults are internal errors or
  `RETRY`, never a wrong verdict.

## Where to look next

- Protocol behaviour (sessions, resume, delivery, limits): [protocol.md](protocol.md).
- Running a host: [operations.md](operations.md).
- Why a thing is the way it is: [decisions.md](decisions.md).
