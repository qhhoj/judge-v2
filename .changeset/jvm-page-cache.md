---
default: patch
---

A JVM run no longer reports the JDK's page cache as its own memory: JAVA, JAVA8 and KOTLIN `preload` the JDK's `lib` (read into the page cache by the judge before each compile and run), so a Java run reports the same memory whether or not the JDK was in the page cache before it (it reported 46 MiB more for a hello world when it was not). PYPY3 preloads its install the same way (a hello world reported 83 MiB instead of 24 MiB when PyPy was not in the page cache); this needs a portable PyPy in a directory of its own, which DEPLOY.md now installs under `/usr/local/lib/pypy`. `check-config` and the daemon's start warn about a missing `preload` path (an apt `pypy3` gets that warning).
