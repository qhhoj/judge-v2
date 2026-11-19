---
default: patch
---

`check-host.sh` no longer fails at random, and an isolate warning can no longer pass: no pipeline ends in `grep -q` under pipefail.
