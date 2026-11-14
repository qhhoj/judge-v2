---
default: patch
---

A run whose case forwarder task ended with a panic or a cancel before the Final now ends INTERNAL_ERROR instead of GRADED, so the bridge never grades it from the cases forwarded so far.
