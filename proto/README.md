# The judge-bridge protocol

`qhhoj/judge/v1/judge.proto` is the contract (package `qhhoj.judge.v1`); its field comments are the reference. The
behaviour around it (sessions, resume, delivery, limits) is [docs/protocol.md](../docs/protocol.md).

## Read scenario 11 first

[`conformance/scenarios/11-grade.yaml`](conformance/scenarios/11-grade.yaml) is one Judging from `Hello` to the stored
Verdict, with every acknowledgement. Every other scenario is a variation on it.

## Lifecycle

```
judge                                   bridge
  Hello {slots, runtimes, in_flight} ->
                                        <- Welcome {session_id, resume[], limits}
                                        <- Assign {ref, problem_code, data_version, language_key, ...}
  Accepted      (seq 1) ->              <- ResultAck {seq 1}
  Compiled      (seq 2) ->              <- ResultAck {seq 2}
  GradingStarted(seq 3) ->              <- ResultAck {seq 3}
  CaseResults*  (seq 4..) ->            <- ResultAck
  Final         (seq n) ->              <- ResultAck {seq n}
```

- Every per-assignment judge message carries `{ref, seq}`; the ack is cumulative. A duplicate is re-acked, a gap closes the
  stream.
- `Abort`, `Ping`/`Pong`, `Draining`, `Disconnect` and the error messages sit around that line.
- Reconnect: the judge sends the same `Hello` with `in_flight` set; `Welcome.resume` answers `CONTINUE(from_seq)` or `DROP`
  per assignment. Scenarios 18 to 20, 24 and 31 show it.
- A source too large for `Assign` (an OUTPUT zip) comes through `FetchSource` (scenario 32).
- Test generation (`GenerateTests` and its calls) is described in the README section "Test generation".

## Scenarios

`conformance/scenarios/*.yaml` are scripted exchanges: setup (judges, problems, submissions), steps (messages in each
direction, API calls, dropped connections, quiet periods) and the expected end state. Each says in `applies_to` whether it plays against the
bridge, the judge or both. The judge runs them in `crates/daemon/tests/conformance.rs` against a scripted fake bridge
(`crates/conformance`), with grading replaced by what the transcript says it produced. The backend vendors this
directory and runs the same files against the real bridge.

## Versions and the files next to the proto

- `version.json`: the package and its current minor. Additive changes bump the minor; a breaking change is a new package.
- `compat/v1.shape.json`: the field numbers and types of every message. CI regenerates it (`bless_shape`) and fails on a
  diff, and fails if the shape breaks against the latest released `proto-v1.*` tag (`check_shape`).
- `golden/*.bin` and `*.json`: one encoded example per message, in both forms. CI regenerates them (`write_golden`) and fails
  on a diff, so an accidental wire change shows up in review.
- `conformance/matcher-cases.json`: the cases for the scenario matcher's wildcards.
