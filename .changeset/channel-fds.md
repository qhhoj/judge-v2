---
default: patch
---

Internal: the sandbox can start a manager and a contestant joined by a sealed shared-memory channel on fds 3-5 (isolate `--inherit-fds`), refusing to start while the daemon holds an inheritable fd. Not used by grading yet.
