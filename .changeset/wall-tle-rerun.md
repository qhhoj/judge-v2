---
default: minor
---

A TLE the wall clock ended, its CPU time within the limit, is judged once more with the host alone, under the same per-submission `limits.tle_reruns` budget: a program blocked on a busy shared disk accrues wall time but no CPU (#95). At most 2 such re-runs per submission, flips counted, and none after one confirmed the TLE, so a program that sleeps or blocks on its own stays TLE after at most one extra run. Such a TLE's case feedback is `wall time`.
