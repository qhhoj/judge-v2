---
default: patch
---

Tests: an isolate test runs a real testlib interactor that traces to stderr before `quitp` and `_pc(n)`, and checks its verdict is still read.
