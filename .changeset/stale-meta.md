---
default: patch
---

Every isolate run removes its box's previous meta file before it starts, and a run that leaves no meta file or an empty one is a sandbox failure (RETRY). Before, an interactive run whose isolate was killed without writing one was graded from the previous run's meta, and an empty meta file read as a clean exit.
