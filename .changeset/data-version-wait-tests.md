---
default: patch
---

Internal: the data-version wait tests move the stamp and the upload marker between two polls through a scripted stamp source instead of a sleeping thread, so a starved host no longer fails them. No runtime change.
