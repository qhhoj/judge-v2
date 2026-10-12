---
default: patch
---

Checkers: a CRLF line break is one break (rstripped no longer sees an extra empty line per CRLF); Java checkers and interactors get their `-Xmx` filled in (the box memory minus 128 MiB) instead of a literal `{memory_mib}` the JVM rejected; the floats checkers compare `inf`/`nan` and overflowing tokens as text, so identical output matches.
