---
default: minor
---

A case that ends TLE on its own, under the hard cap, is judged once more while no other run is on the host, and that result is the case's (COR-48: CPU time under load ran up to x1.46 for cache-bound code). Every isolate run, `selftest`'s too, holds a host-wide run gate shared; the re-run holds it exclusive. At most `limits.tle_reruns` (default 2, 0 turns them off) re-runs per submission may confirm a TLE; one that flips the verdict is free. A contestant run is now killed at max(TL + 0.5 s, 1.7 x TL) of CPU instead of at the first tick past the limit (`--extra-time` is absolute), so a TLE reports its real time; a run past the time limit is TLE even if the cgroup then OOM-killed it.
