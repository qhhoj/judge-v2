---
default: patch
---

Bound memory use: init.yml output_limit_length is capped at 64 MiB, the metrics listener limits and times out connections, the runtime version probe cannot hang on a leaked pipe, and an uploaded output zip is released once written to disk.
