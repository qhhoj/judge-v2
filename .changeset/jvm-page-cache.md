---
default: patch
---

A JVM run no longer reports the JDK's page cache as its own memory: JAVA, JAVA8 and KOTLIN `preload` the JDK's `lib` (read into the page cache by the judge before each compile and run), so a Java run reports the same memory whether or not the JDK was in the page cache before it (it reported 46 MiB more for a hello world when it was not). PYPY3 preloads its install the same way (a hello world reported 83 MiB instead of 24 MiB when PyPy was not in the page cache); the unit sets `MemoryLow=512M` so the preloaded pages stay resident; `check-config` warns about a missing `preload` path.
