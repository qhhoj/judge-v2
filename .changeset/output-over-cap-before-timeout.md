---
default: patch
---

Output limit: a run whose output went over the limit is OLE even when it then ran out of time. The JVM ignores SIGXFSZ and `System.out` and `PrintWriter` swallow the write error, so a Java or Kotlin program printing in a loop ran on after its output was cut at the limit and was shown as TLE. Tests: the C and C++ memory-limit test programs keep every block they allocate, so clang no longer deletes the allocation.
