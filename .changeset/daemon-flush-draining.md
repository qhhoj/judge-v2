---
default: patch
---

At stop the daemon ends its session stream after `Draining` and waits for the bridge to end the session, so an idle drain no longer loses `Draining`.
