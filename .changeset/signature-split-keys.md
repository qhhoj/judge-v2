---
default: patch
---

init.yml `signature_grader` accepts `split` and `cores`; `split` with `allow_main`, `cores` without `split` or outside 1..2, and `split` together with `interactive`, `output_only` or `file_io` are refused. Until split signature is graded, a split problem answers `RETRY UNSUPPORTED` (`signature.split`, `signature.split.paired`) instead of an internal error. Legacy files parse exactly as before.
