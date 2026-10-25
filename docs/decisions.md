# Decisions

One line per decision: what, and why.

- The judge dials the bridge over gRPC with TLS: one typed contract for both sides, HTTP/2 backpressure, and the bridge stays reachable only from judge hosts.
- Changes inside protocol v1 are additive only: judges and bridges of different builds must keep talking.
- Results are numbered per assignment and acked cumulatively: a reconnect replays the outbox without double-applying a write.
- A resumed assignment does not spend the requeue: a short network drop must not count as a lost judge.
- `RETRY UNSPECIFIED` is capped at three per submission: a problem no judge can serve must end as an internal error, not loop. Transient reasons (DATA_UNAVAILABLE, RUNTIME_MISSING, SANDBOX_FAILURE) wait with a backoff outside the cap and end IE after the 30-minute deferral window, and UNSUPPORTED is a refusal: a data save or a judge hiccup is not a failure, and must not turn queued submissions into IE.
- Every problem-author program (checker, interactor, generator, signature entry) runs in isolate: problem data is not trusted.
- Output over the cap is OLE ahead of TLE: the JVM ignores SIGXFSZ and would otherwise run a print loop to the time limit.
- An unknown init.yml key is refused as UNSUPPORTED; an unknown checker arg is ignored, because refusing needs a production query for stray args first.
- Python 2 and 32-bit assembly are not supported: Python 2 is end of life and isolate's seccomp kills 32-bit binaries.
- Scratch is not supported: it needs its own runtime, and its problems are refused with `RETRY RUNTIME_MISSING`.
- The vendored testlib is C++ only: Pascal and Java checkers that include testlib do not compile.
- A contestant's unreadable OUTPUT zip is WA on every case: it is the contestant's mistake, not a judge fault.
- Releases publish only from a signed tag on main (2026-10-24, bug hunt infra): `packaging/check-release-tag.sh` gates the GitHub Release and the `:vX.Y.Z` image on an annotated `vX.Y.Z` tag whose signature GitHub verified, pointing at the built commit, which is on main. The tarball and every pushed image carry a build provenance attestation (`actions/attest-build-provenance`); installs verify it (`gh attestation verify`, DEPLOY.md step 8). Why: any `v*` tag published an unsigned release that hosts checked only against a checksum from the same release (P2-4).
- The image build pins protoc and `rustup-init` by sha256, and CI passes `github.base_ref` to scripts through `env` only. Why: downloads ran unchecked, and an expression in `run:` is interpolated into the shell (P3-2, P3-4).
- `check-config` runs the daemon's config, pinning and disk-bound checks without starting it, probing the quota on a spare box (selftest covers the languages file): an install runs it before switching, so a config these checks refuse fails before the switch, not in a restart loop.
- A conformance step waits 5 s of time the host ran the test, not 5 s of wall clock (extension capped at 60 s): a starved host must not fail a scenario, and a hung peer on a healthy host still fails in 5 s.
- The conformance replay reports each transcript `cases` message with one `Reporter::cases` call, which the daemon batches as one arrival: the batch age (100 ms) must not split a scripted message on a starved host. Grading reports case by case and never calls it.
- The standard checker's whitespace is C `isspace` (space, \t, \n, \v, \f, \r): the legacy checker split on it, so a rejudge of a migrated submission whose output uses a vertical tab must stay AC.
- An assignment with a time limit outside (0, 3600] s (zero, negative, NaN) or a memory limit outside [1, i32::MAX] KiB ends as an internal error before grading: a zero time limit leaves only the wall clock bounding a run, and it is never graded without a CPU limit.
- The sorted, easy, linecount and rstripped checkers use the same byte whitespace as standard (C `isspace`, with \v): the legacy versions split, stripped and removed whitespace on bytes with exactly that set, so \x1c-\x1f stay ordinary bytes.
- The linecount checker counts a whitespace-only line as a line (compared trimmed, so as empty), as the legacy checker did: only truly empty lines are dropped, so `a` and `a` plus a spaces-only line differ.
- A missing `<root>/<code>` directory is `DataError::Unavailable` (`RETRY DATA_UNAVAILABLE`), not an i/o error (COR-18): the data is not there yet (not synced, or a rename the bridge has not caught up with), so the run is retried with fresh inputs instead of ending as IE.
