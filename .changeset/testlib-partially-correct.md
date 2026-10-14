---
default: patch
---

Checkers: testlib `_pc(n)` earns n/200 of the case (Polygon's 0..200 scale), read from testlib's `partially correct (n)` stderr line checked against the exit code. testlib built without `TESTSYS` exits with n itself, so `_pc(0)` was full marks, `_pc(1)`/`_pc(2)` WA/PE and most other n IE; `_pc(0)` is now a wrong answer worth 0.
