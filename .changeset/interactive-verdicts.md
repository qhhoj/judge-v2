---
default: patch
---

Interactive problems: the interactor ignores SIGPIPE, so a last write after the solution exited is no longer
IE; a solution that fails on EOF or a broken pipe after the interactor quit with WA or points gets the
interactor's verdict; an interactor that runs out of its limits, or whose CPU use made the solution time out
on the wall clock, is IE instead of the contestant's TLE.
