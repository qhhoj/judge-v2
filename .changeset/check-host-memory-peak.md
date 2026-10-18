---
default: patch
---

`check-host.sh` fails a host whose isolate cgroup has no `memory.peak` (kernel before 5.19). There isolate reports no cgroup memory, and the memory limit is held to the peak RSS alone, which counts a runtime's shared objects as the program's.
