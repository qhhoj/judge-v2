# Grading

How one assignment is graded. The code path with file names is in [architecture.md](architecture.md).

## Assignment lifetime

`Assign` -> `Accepted` -> fetch the source (inline, or `FetchSource` for a file-only language) -> resolve the problem
(cache) -> compile -> run the cases -> `Final` -> free the slot. Every message goes through the assignment's outbox
(see "Delivery" in [protocol.md](protocol.md)). The slot is freed before the bridge can see the `Final` and send the
next `Assign`, so a fast bridge never meets `SLOTS_FULL`.

## Verdict rules

Case statuses, as the bridge stores them:

| Code | Meaning |
|---|---|
| AC | accepted |
| WA | wrong answer |
| TLE | time limit exceeded |
| MLE | memory limit exceeded |
| OLE | output limit exceeded |
| RTE | runtime error: killed by a signal, or an uncaught exception (the feedback names it) |
| IR | invalid return: the program exited with a non-zero code (the feedback is `exit code N`) |
| SC | short-circuited: skipped because an earlier case or batch already decided the outcome |

`sandbox/src/classify.rs` turns the isolate meta file of one run into a verdict, checking in this order:

1. isolate status `XX` (its own failure) is an internal error for the case and the assignment.
2. **MLE:** `cg-oom-killed`, or the program's own memory over the limit.
3. **OLE:** more bytes written than the output cap, or a SIGXFSZ exit. This is decided ahead of TLE, because the JVM
   ignores SIGXFSZ and swallows the write error, so a print loop would otherwise run on to the time limit.
4. **TLE:** isolate status `TO` (CPU or wall time).
5. status `SG` (killed by another signal) is RTE with the signal name; status `RE` (non-zero exit) is IR.
6. Otherwise the run is fine and the checker decides AC, WA or partial points.

A run killed at the time limit is TLE, not MLE, unless the cgroup OOM-killed it.

Time is the CPU time from the meta file; wall time only stops sleeping or blocked programs.

### Memory

The program's memory is `Meta::memory_kib` (`sandbox/src/meta.rs`): the cgroup peak (`cg-mem`) less the page cache of the
run's own input and output files, but never below the peak resident set (`max-rss`) and never above the cgroup peak.
Pages the program holds without mapping them (tmpfs, memfd, pipes) therefore count; page cache of its own files does
not. An OOM kill reports the cgroup peak; a run that timed out reports the smaller of the two counters. When isolate reported one counter only, that one is used.

The cgroup limit is the memory limit plus room for the page cache of what the run writes (one output cap, at most
64 MiB, `spec::cgroup_memory_kib`), so a small-memory program with a large output is not OOM-killed for its page cache.

The kernel charges a page-cache page to the cgroup that first reads it in, so a run that is first to read its
runtime would be charged for it: a JVM 46 MiB for `libjvm.so`, the CDS archive and `lib/modules` after a reboot or
memory pressure evicted them, a PyPy 58 MiB for libpypy and the stdlib it imports at start. Before each compile and
run the judge reads a language's `preload` paths (`languages.toml`: the JDK's `lib` for JAVA, JAVA8 and KOTLIN, a
portable PyPy's `bin` and `lib` for PYPY3; `{runtime}` is the install above the run command's real file) into the
page cache itself (`sandbox/src/preload.rs`), so those pages are charged to the judge and a run reports the same
memory whatever ran on the host before it. A page evicted between that read and the run's first touch of it is
still charged to the box (a short window: the read is right before every compile and run). PYPY3 lists both the
portable layout and Ubuntu's `pypy3` package (`libpypy3.9-c.so`, `/usr/lib/pypy3.9`); `check-config` and the daemon's
start warn when none of an installed language's `preload` paths exists. PY3 and RUBY preload Ubuntu's interpreter, shared library and stdlib (7 and 5 MiB for a hello world); an entry whose last component holds `*` (`libruby*.so*`) stands for the directory entries that match it, so no version is written. With the runtime already cached the preload costs about 13 ms per run for PY3 and 26 ms for RUBY (5.6 ms for the JDK, same method: median of 100 calls after warm-up), once per compile and once per run.

JVM languages (`jvm = true`) get the memory limit raised by `limits.jvm_reserve_kib` (default 128 MiB, in the config), for
the MLE check and the cgroup alike, with `-Xmx` equal to the problem's memory limit. A `java.lang.OutOfMemoryError` on stderr turns RTE or IR into MLE. Jury programs
that are JVM languages keep a fixed 128 MiB reserve outside their heap.

### Uncaught exceptions

An uncaught exception exits non-zero, which isolate calls `RE`; the old judge reported these as RTE, not IR. A
language that sets `exception_marker` in `languages.toml` has its stderr kept, and a non-zero exit carrying the marker
becomes RTE named by the exception.

### Output limits

`output_limit_length` (bytes; the case's value, else the problem's, else `limits.default_output_limit`) becomes
isolate's `--fsize`, rounded up to KiB plus 1 KiB of slack. Only `output_prefix_length` bytes (at most 512) of the output are ever sent back.

## Cases, batches and points

- A case that is not AC skips the rest of its batch (SC). With `short_circuit`, any case that is not AC skips every
  later case. Even without `short_circuit`, a failed case or batch worth 0 points stops all later cases
  (`problem/src/short_circuit.rs`). A pretest run grades only `pretest_test_cases`.
- Each case reports `points` and `total`. A batched case carries the batch total; the bridge scores a batch by its
  weakest case.
- A refused feature (generator, `nobigmath`, a checker kind not graded here, a non-testlib interactor, per-case checker
  overrides) is `Final{RETRY, UNSUPPORTED}`. A language with no runtime on this judge is `RETRY RUNTIME_MISSING`.
- A compile error ends the assignment with `Final{COMPILE_ERROR}`.

## Box lifecycle

Worker *i* owns two boxes: `box_base + 2i` (the solution) and `box_base + 2i + 1` (checker, interactor or compile
helper). The worker is pinned to one core.

1. `isolate --cg --init`, then put the source or compiled binary in the box.
2. Compile inside the solution box with the compile limits (CPU 10 s, wall 20 s, 1 GiB, 32 processes, 64 MiB output;
   a language may override them with `compile_limits`).
3. For each case: only that case's input goes into the box, `isolate --run`, read the meta file, move the output out,
   check it, delete the per-case files. Every run starts from a fresh `--init`; the binary is put in again.
4. `isolate --cleanup` always runs, also on abort, panic or drain. At daemon start every owned box is cleaned.

Per run: no network (an empty namespace), default directories read-only, the only extra mounts are read-only runtime trees
(`dirs` in `languages.toml`), `--open-files=64`, `--env=PATH` and `HOME=/box`. Solution stderr goes to `/dev/null`
except for languages that need it (see above). The solution box sees only its own input: answers and other cases never
enter it.

## Checkers

- **Built-in** (`standard`, `floats`, `floatsabs`, `floatsrel`, `identical`, `linecount`): native Rust over the solution
  output (already capped) and the answer, with no sandbox. `unicode` in `hints` makes token comparison UTF-8 aware.
- **Custom (`bridged`)**: compiled once per content hash into the cache, run in the worker's second box with `input`,
  `output` and `answer` copied in read-only. Limits: time max(10 s, 2 x TL), 1 GiB, one process, no network.
  - testlib (checkers and interactors): built with `-DTESTSYS` and run with a fifth argument, `.result.txt`, a file
    the judge creates empty in the freshly staged box (a program file of that name is IE). testlib writes its quit
    message there; the verdict is the exit code: 0 AC, 1 WA, 2 and 4 presentation error (WA), 8 unexpected EOF
    (WA), 3 fail (IE), 7 partial: the number that starts the message (`quitp` and `quitf(_points, ...)` alike) as
    points of the case (percent with `treat_checker_points_as_percentage`; outside the case, or not a number, IE);
    a program that leaves the result file empty gives them as a last stderr line of exactly `points <number>`,
    50 + n `_pc(n)`: n / 200 of the case, when stderr ends with testlib's own result-file line. Any other code is
    IE. Apart from that `points` line, nothing the program prints on stderr decides the verdict.
  - `default` uses the documented exit-code convention of the old judge: 0 AC, 1 WA, 2 the share that starts
    stdout.
  - The feedback is at most 50 bytes, the details 512: testlib's message after the program's stderr trace; the
    first stderr line for `default`. Each stream is read through a 64 KiB window, whole lines only, at the end that
    holds what is read (themis stdout and testlib stderr: the end; `default`: the start).
- **Interactor** (`interactive`): two isolate runs started together, joined by two pipes (solution stdout to interactor
  stdin and back). The interactor gets `input` and `answer`; its wall time is the solution's plus 2 s, CPU max(10 s,
  2 x TL). The interactor's verdict counts only when the solution ended normally: a solution TLE, MLE, RTE or IR wins.
  If the interactor dies (exit 3 or a signal), the case is an internal error.
- Generators and signature graders always run in isolate too.

### Split signature

A `signature_grader` with `split: true` runs the author's `entry` and the contestant's functions in two boxes,
joined by two pipes (fds 3/4), with C++ glue generated from the problem header (`crates/interface`). The
contestant box holds only the submission's compiled program; the jury box runs `entry` with its stdin `input`
and stdout `output`, exactly as the one-process grader had them, so the problem's checker reads the same bytes.
Roles (which side implements each function) come from the compiled entry: a symbol the entry defines is a
callback the contestant calls, one it references is a function the contestant implements. The jury is built
once per language key (compiler parity with the submission) and cached; a failed build is never cached (a
missing tool is RETRY, a grader that does not build is IE). C submissions are CE (the interface is C++).

Time is CPU, as one process counted it: the contestant is charged its own box's CPU plus the jury CPU spent in
the callbacks it called. Memory is the contestant box's own. The jury runs under its own limits (CPU
max(10 s, 2 x TL), memory max(1 GiB, ML)). Verdict, first match: the contestant over its memory or output limit
-> MLE/OLE; charged time over the TL -> TLE; its wall clock ran out -> IE when the jury's own CPU used the
waiting time, else TLE; a protocol violation the grader's glue recorded -> WA; the contestant crashed or exited
non-zero (any code) -> RTE/IR; the jury over its time limit -> IE; the jury over its memory, killed or non-zero
-> that verdict; otherwise the problem checker on the jury's output.

Differences from one process, by design: each value-returning call costs one pipe round trip of contestant CPU
(about 3 us on an idle host, more under load; a problem switches only after the gate in `docs/decisions.md`);
a `const&` result is the jury object's live value, one copy per object, compared and re-sent when changed, so
the object must stay alive for the whole run (one the grader frees or moves later, e.g. a row of a vector that
grows, gives RTE or garbage even if the contestant no longer uses it); a
`const&` parameter of a contestant function is a copy taken at the call (a change the jury makes to that object
during the call is not seen); a callback from a contestant static constructor aborts (RTE), and `exit` there
does not flush the grader; an exception never crosses between the two sides (an uncaught one aborts its side).
`split: false` keeps the one-process path.

## Languages

Languages are data in `languages.toml`; its header explains the placeholders. TEXT has no runtime: the source is the
output of every case. OUTPUT is a zip of outputs read per case by name; a zip that cannot be read is WA on every case
(the contestant's mistake, not an internal error). Both are file-only through `FetchSource`.

## Caching and data version

- The bridge sends `problem.data_version` in `Assign`. The backend writes `<code>/.data-version` last, after init.yml and
  after any upload (temp file plus rename).
- Problem cache key: `(code, data_version)`. A missing stamp (legacy data) means grade with what is on disk, report
  `data_version_used = 0`. A stamp lower than assigned (a lagging NFS) waits up to `data_wait_ms` (default 10 s) then sends
  `Final{RETRY, DATA_UNAVAILABLE}`. A higher stamp uses the newer data. A missing `<code>` directory (data not synced
  yet, or a renamed code) is `Final{RETRY, DATA_UNAVAILABLE}` at once, never an internal error.
- Grading reads the data root after resolve, so the upload marker and `.data-version` are read again at the end: a moved
  stamp turns the result into `RETRY DATA_UNAVAILABLE`.
- Archives are extracted under zip limits (no traversal, symlinks or lying sizes) into `cache_dir`. Problem directories are
  opened without following symlinks (`O_NOFOLLOW`); case names may be `/`-separated relative archive paths.
- Compiled checkers and interactors are cached by `sha256(source bytes, language, compiler build)`. Both caches have an
  LRU cap (`cache_max_bytes`) and never evict an entry an active worker holds. User programs are never cached.

## file_io

`file_io.input` and `file_io.output` must be plain file names: non-empty, no leading `.`, no `/` or `\`, and not `input`,
`output`, `main` or `.stderr`. The backend refuses them on upload; the judge refuses them with an internal error.
