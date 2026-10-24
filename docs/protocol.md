# Judge-bridge protocol

The contract is `proto/qhhoj/judge/v1/judge.proto` (package `qhhoj.judge.v1`). The field comments there are the
reference for each message; this page is the behaviour around them. The lifecycle with its scripted examples is in
[`proto/README.md`](../proto/README.md).

## Transport and auth

- gRPC over TLS. The judge dials the bridge; the bridge listens only for judge hosts. One bidirectional stream,
  `JudgeLink.Connect`, carries everything; `FetchSource` (and the generation bundle calls) are separate server-streaming
  calls on the same channel.
- TLS is mandatory unless the bridge URL is loopback (dev and e2e). The judge pins the bridge's CA (`tls.ca_file`)
  and checks the server name.
- Credentials are gRPC metadata `x-judge-name` and `x-judge-key`, checked by the bridge before the stream handler
  runs. The key is read from `key_file` at each connect, so rotating it needs no restart; a rotate or block makes
  the bridge close the stream.
- `Hello` must be the first message within 15 s; anything else closes the stream.

## Versioning

- `qhhoj.judge.v1` is frozen for breaking changes. Changes inside v1 are additive only: new fields, new oneof members,
  new enum values. A receiver ignores what it does not know.
- `Hello.protocol_minor` and `Welcome.protocol_minor` (the lower of the two sides) gate optional behaviour. The
  current minor is in `proto/version.json`. `Hello.features` names what this judge grades beyond plain programs
  (`daemon/src/wire.rs` `FEATURES`).
- A judge that meets a feature it lacks answers `Final{RETRY, UNSUPPORTED}`.
- CI keeps this honest: `proto/golden` binaries must match their JSON, `proto/compat/v1.shape.json` must be current,
  and the shape must not break against the latest released `proto-v1.*` tag.
- A breaking change is a new package (`v2`) served side by side for one release.

## One assignment

`Assign` -> `Accepted` -> `Compiled` -> `GradingStarted` -> `CaseResults`* -> `Final`, each acknowledged by `ResultAck`.
Every per-assignment judge message carries `{AssignmentRef ref, uint32 seq}`: `seq` starts at 1 and grows by 1 per
message. The reference is `{submission_id, generation}`.

Batch boundaries are carried inline (`Case.batch`: 0 = none, else the 1-based batch) instead of begin and end
messages, so a replayed or resumed message cannot lose batch state.

## Flow control

- **Bridge to judge (credit):** the bridge never has more than `slots` assignments outstanding on a session. An `Assign`
  beyond that is refused by the judge with `SLOTS_FULL`, which the bridge treats as a protocol error.
- **Judge to bridge:** the bridge handles one message per session at a time and reads the next only after its database
  write, so the HTTP/2 window fills and the judge's send blocks. The judge's outbox absorbs this up to 8 MiB; above
  that, workers pause before starting the next case.
- **Coalescing:** case results are flushed every 100 ms or 64 cases, at most 256 per message
  (`daemon/src/batcher.rs`).

## Reconnect and resume

- **Judge side:** reconnect with jittered exponential backoff (500 ms up to 30 s, `reconnect` in the config). Running
  assignments keep running and their outboxes keep unacked messages. `Hello.in_flight` lists every `ref` with the
  last `seq` sent.
- **Bridge on disconnect:** assignments are held in grace for `resume_grace_ms` (default 30 s). If the same judge name
  reconnects with a matching `ref` (same generation, row still pending or grading), `Welcome.resume` says
  `CONTINUE(from_seq)` and the judge replays its outbox from there. This does not spend the requeue. Otherwise the
  answer is `DROP` and the judge kills that run. When grace expires the bridge requeues once; a second loss is an
  internal error.
- **DROP, abort and rejudge:** the judge ends every run it kills, including a `DROP` and an abort before `Accepted`,
  with `Final{ABORTED}`. The bridge keeps that run's slot counted ("winding") until that `Final` arrives, and ignores a
  late `Accepted` for a run it released instead of closing the session. A stale-generation write on a live session
  winds its run down the same way.
- **Unacknowledged `Assign`:** if `Accepted` does not arrive within 20 s the assignment goes back to the queue at once
  without spending the requeue, but its slot stays busy until the judge's `Final{ABORTED}` (or the session ends).
- **Bridge restart:** judges are accepted at once; rows no judge resumes are reset by the bridge's resync after two ticks,
  without spending the requeue.
- **Abort while in grace:** the bridge records it; on resume it sends `Abort`, on expiry it marks the submission aborted
  instead of requeueing.
- A second connection with the same judge name replaces the first. With resume this is the normal reconnect path.

## Delivery: at-least-once, exactly-once effect

- The submission has a `judgeGeneration` (incremented on every claim) and a `judgeSeq` (reset at claim).
  `Assign.ref.generation` is the claimed generation.
- Every judge-driven write filters on the generation and `seq - 1` and sets `seq` in the same update. A duplicate
  (`seq` at or below the stored one) matches nothing and is re-acked. A gap is a protocol error. A write from an older
  generation (before a requeue or rejudge) is a no-op, and the bridge sends `Abort` for that ref.
- After each applied or duplicate message the bridge sends `ResultAck{ref, seq}` (cumulative) and the judge trims its
  outbox. `Final` is acked only after its write and the graded-job enqueue, so a crash between them resends `Final`,
  which is then a no-op. The graded job id is `submissionId:generation`.
- `Final{RETRY}` (data unavailable, runtime missing, unsupported feature, sandbox failure) returns the row to the queue
  without spending the requeue and bans that judge for one resync tick. The third `RETRY` makes the row an internal
  error (a cap against poison retries).
- `Final.data_version_used` is stored on the submission (see "Caching and data version" in [grading](grading.md)).

## Limits

The bridge sends its limits in `Welcome.limits`; the judge stays inside them, and they can only lower these defaults.

| Limit | Value |
|---|---|
| Message size (bridge receive) | 4 MiB; `Hello` 256 KiB |
| Connections | at most 256 total, at most 8 unauthenticated per address |
| Streams per connection | one `Connect` plus at most `slots` concurrent `FetchSource` |
| Rate | at most 1000 messages/s per session, then the stream is closed |
| Cases | at most 256 per message, at most 10000 per submission |
| Text | log 64 KiB; feedback 50 B; extended feedback and output prefix 512 B (clipped, not rejected) |
| Inline source | at most 1 MiB; a file-only submission comes through `FetchSource` |
| Timings | `Hello` 15 s, `Accepted` 20 s, ping 10 s, idle 60 s (paused while a message is handled), resync 30 s, grace 30 s |

## Source and generation transfer

`FetchSource` streams a file-only submission (an OUTPUT zip) in 1 MiB chunks, only for a `ref` the calling session
holds; judges need no HTTP egress to the site. Test generation (`GenerateTests`, `FetchGenerationBundle`,
`UploadGeneratedTests`, `GenerationDone`) is described in the README section "Test generation".

## Dev-only gates

Setting `control_dir` in the config (loopback bridges only) makes the daemon pause at three points while a hold file
exists in that directory: `hold-ack` before `Accepted`, `hold-begin` before `GradingStarted`, `hold-end` before `Final`.
The real-backend end-to-end journey uses them to abort or rejudge a submission mid-run.
