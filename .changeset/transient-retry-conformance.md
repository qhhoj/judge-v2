---
default: patch
---

Protocol contract: RetryReason documents how the bridge treats each reason, conformance 27 pins the RETRY cap on UNSPECIFIED (a failure), and the new 27b pins that transient reasons (DATA_UNAVAILABLE, RUNTIME_MISSING, SANDBOX_FAILURE) never spend it and end as IE only after the bridge's deferral window. The judge sends the same messages as before.
