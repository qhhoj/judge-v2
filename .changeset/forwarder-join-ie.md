---
default: patch
---

A run whose case forwarder task ended with a panic or a cancel before the Final now ends INTERNAL_ERROR instead of GRADED (a RETRY or compile error is kept and logged), so the bridge never grades it from the cases forwarded so far.
