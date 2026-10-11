---
default: minor
---

`checker.themis`: bridged checkers with `type: themis` read the test and contestant directories from stdin, find the case files under the problem's file I/O names and the checker source's `.inp`/`.out` literals (every letter case), and print the 0..1 score on the last stdout line.
