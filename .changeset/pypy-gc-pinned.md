---
default: patch
---

PyPy 3 runs pin `PYPY_GC_NURSERY=4M` and `PYPY_GC_MAX_DELTA=2GB`, so a PyPy verdict near the memory limit no longer depends on the judge host's CPU cache (PyPy sized its nursery from 1 MiB to 130 MiB) or RAM.
