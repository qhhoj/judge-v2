---
default: patch
---

Internal: `Reporter::cases` reports several cases as one arrival for the daemon's batcher (default: one `case` call each). Only the conformance replay uses it, so a transcript's Cases message is no longer split by the 100 ms batch age on a starved host. Grading is unchanged.
