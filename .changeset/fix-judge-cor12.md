---
default: patch
---

The standard checker splits tokens on vertical tab as legacy did, and the daemon refuses assignments with a zero, negative, non-finite or oversized limit.
