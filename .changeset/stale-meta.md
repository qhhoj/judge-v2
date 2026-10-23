---
default: patch
---

Every isolate run removes its box's previous meta file before it starts, and a run whose meta file is missing, empty or has neither `time` nor `status` is a sandbox failure (RETRY). Before, an interactive run whose isolate was killed before writing its meta file was graded from the previous run's meta, and an empty meta file read as a clean exit.
