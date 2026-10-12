---
default: patch
---

Languages: C/C++ compile with `-DONLINE_JUDGE` and Pascal with `-dONLINE_JUDGE`, so `#ifndef ONLINE_JUDGE` freopen templates use stdio; PYPY3 syntax-checks with `py_compile` so a syntax error is CE, not IR; Java main-class detection scans top-level types by brace depth (a braced member before `main` or an inner `public class` no longer misnames the class); a compiler that cannot exec in the box is RETRY RUNTIME_MISSING, not CE.
