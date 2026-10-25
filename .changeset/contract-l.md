---
default: patch
---

Archives the backend would read differently are refused (another central directory, ambiguous Unicode paths, control characters in names); `check-config` and the daemon refuse a `cache_max_bytes` below 4 GiB, and `check-config` a missing `problem_data_root`.
