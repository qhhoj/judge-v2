---
default: patch
---

PyPy runs with `PYPY_GC_NURSERY=4M`. PyPy sizes its nursery from the `/proc/cpuinfo` cache size, so a PyPy run's memory depended on the judge host's CPU: ~130 MiB on a Xeon 8573C (an output flood was OOM-killed under a 64 MiB limit), the same as 1M on an EPYC.
