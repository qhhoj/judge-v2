---
default: patch
---

PY3 and RUBY `preload` their interpreter, shared library and standard library, so a run reports the same memory whether or not they were in the page cache before it (a hello world reported about 7 MiB more for CPython and 5 MiB more for Ruby when they were not). A `preload` entry's last component may hold `*` (`libruby*.so*`) to match directory entries by name, without writing a version. With the runtime already cached the preload costs about 13 ms per run for PY3 and 26 ms for RUBY.
