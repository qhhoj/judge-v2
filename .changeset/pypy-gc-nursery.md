---
default: patch
---

PyPy runs with `PYPY_GC_NURSERY=4M`. PyPy sizes its nursery as half the `/proc/cpuinfo` cache size, so a PyPy run's memory depended on the judge host's CPU (130 MiB nursery on a Xeon with 260 MiB L3: an output flood was OOM-killed under 64 MiB).
