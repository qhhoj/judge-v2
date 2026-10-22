---
default: patch
---

Memory limit: the program's own memory is the cgroup peak less the page cache of its own input, output and kept stderr (output files counted by allocated blocks), never under the peak RSS and never over the cgroup peak. Before this it was the smaller of the two counters, so 60 MiB of heap plus 22 MiB written to `/dev/shm` (or a memfd) and read back unmapped showed 62 MiB and passed a 64 MiB limit. A timed-out run, and compilers and jury programs, keep the smaller counter. Runs no longer get a writable `/dev/shm`.
