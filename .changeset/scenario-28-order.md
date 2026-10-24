---
default: patch
---

Conformance scenario `28-accept-timeout` assigns, accepts and acks s2 before it assigns s1, so a slow bridge write can no longer make s1 time out before s2's ack. Same assertions and end state; no runtime change.
