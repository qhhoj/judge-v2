---
default: minor
---

The daemon refuses to start when nothing bounds what runs write to isolate's box root: `[isolate] disk_quota` must be set and settable, or the box root must be a filesystem of its own (`allow_unbounded_disk` for loopback dev stacks only). A configured quota isolate cannot set is now a start error, not a warning. JAVA, JAVA8 and KOTLIN pin `-XX:MaxRAM` (javac too), so the JVM heap no longer depends on the host's RAM. The conformance harness no longer races a run's start against a drop, a skipped run or a drain.
