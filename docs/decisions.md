# Decisions

One line per decision: what, and why.

- The judge dials the bridge over gRPC with TLS: one typed contract for both sides, HTTP/2 backpressure, and the bridge stays reachable only from judge hosts.
- Changes inside protocol v1 are additive only: judges and bridges of different builds must keep talking.
- Results are numbered per assignment and acked cumulatively: a reconnect replays the outbox without double-applying a write.
- A resumed assignment does not spend the requeue: a short network drop must not count as a lost judge.
- `RETRY` is capped at three per submission: a problem no judge can serve must end as an internal error, not loop.
- Every problem-author program (checker, interactor, generator, signature entry) runs in isolate: problem data is not trusted.
- Output over the cap is OLE ahead of TLE: the JVM ignores SIGXFSZ and would otherwise run a print loop to the time limit.
- An unknown init.yml key is refused as UNSUPPORTED; an unknown checker arg is ignored, because refusing needs a production query for stray args first.
- Python 2 and 32-bit assembly are not supported: Python 2 is end of life and isolate's seccomp kills 32-bit binaries.
- Scratch is not supported: it needs its own runtime, and its problems are refused with `RETRY RUNTIME_MISSING`.
- The vendored testlib is C++ only: Pascal and Java checkers that include testlib do not compile.
- A contestant's unreadable OUTPUT zip is WA on every case: it is the contestant's mistake, not a judge fault.
- A conformance step waits 5 s of time the host ran the test, not 5 s of wall clock (extension capped at 60 s): a starved host must not fail a scenario, and a hung peer on a healthy host still fails in 5 s.
