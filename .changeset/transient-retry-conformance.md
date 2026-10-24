---
default: patch
---

Protocol contract: RetryReason documents how the bridge treats each reason. Conformance 27 pins the RETRY cap on UNSPECIFIED (a failure). The new 27b pins that transient reasons (DATA_UNAVAILABLE, RUNTIME_MISSING, SANDBOX_FAILURE) never spend it, 27c that they end as IE once the bridge's deferral window has passed, and 27d that a transient RETRY between failures neither spends nor resets the cap. No scenario depends on how fast a run cycle is. The protocol and decision docs state the new rule. The judge sends the same messages as before.
