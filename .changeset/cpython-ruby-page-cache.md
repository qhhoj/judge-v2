---
default: patch
---

PY3 and RUBY `preload` their interpreter, shared library and standard library, so a run reports the same memory whether or not they were in the page cache before it (a hello world reported about 7 MiB more for CPython and 5 MiB more for Ruby when they were not). A `preload` entry may end in a name and `*` (`/usr/lib/python3*`) to match the entries of its directory by prefix, without writing a version.
