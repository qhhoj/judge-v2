---
default: patch
---

An archive problem whose data version changes while it is graded is retried, as a loose-file problem already was: its checker, interactor, signature and file I/O sources are read from the data root after resolve, so the result could mix two versions under the old version number.
