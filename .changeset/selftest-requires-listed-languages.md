---
default: minor
---

`selftest` requires every language of the languages file that has conformance rows, unless `languages.toml` marks it `selftest_optional = true` (BF). The `--require` flag is removed: drop it from deploy scripts.
