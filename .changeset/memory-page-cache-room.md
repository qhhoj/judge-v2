---
default: patch
---

Memory limit: the cgroup gets room above the memory limit for the page cache of the files a run writes (one `--fsize` worth, at most 64 MiB), and MLE is the program's own memory over the limit, or an OOM kill at the raised cgroup limit. The program's own memory is the smaller of the cgroup peak, which also counts the page cache of written output, and the peak RSS, which also counts mapped files already in page cache (PyPy's ~37 MiB libpypy), or the one isolate reported when it reported only one. A run killed at the time limit is not MLE unless the cgroup OOM-killed it: on a timeout isolate reports its own proxy's peak RSS. Before this, the page cache of a large output counted against a small memory limit, so a run could be OOM-killed and shown as MLE while the program itself used 2 MiB. Reported memory is the program's own; an MLE shows the larger of the cgroup peak and the peak RSS, so it is never shown under the limit.
