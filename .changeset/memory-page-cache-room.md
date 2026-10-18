---
default: patch
---

Memory limit: the cgroup gets room above the memory limit for the page cache of the files a run writes (one `--fsize` worth, at most 64 MiB), and MLE is decided by the program's peak RSS over the limit, or by an OOM kill at the raised cgroup limit. Before this, the page cache of a large output counted against a small memory limit, so a run could be OOM-killed and shown as MLE while the program itself used 2 MiB. Reported memory is unchanged: the program's own memory, and the cgroup peak for an MLE.
