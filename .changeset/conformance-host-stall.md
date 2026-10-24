---
default: patch
---

Internal: a conformance step's 5 s wait no longer counts time the host did not run the test process (at most 60 s more), so a starved CI host no longer fails a scenario. No runtime change.
