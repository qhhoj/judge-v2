---
default: patch
---

Memory limit: the cgroup gets room above the memory limit for the page cache of the files a run writes (one `--fsize` worth, at most 64 MiB), and MLE is decided by the program's own memory over the limit (the smaller of the cgroup peak and the peak RSS), or by an OOM kill at the raised cgroup limit. Before this, the page cache of a large output counted against a small memory limit, so a run could be OOM-killed and shown as MLE while the program itself used 2 MiB. Reported memory is the program's own; an MLE shows the larger of the cgroup peak and the peak RSS, so it is never shown under the limit.
