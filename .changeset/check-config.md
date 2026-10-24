---
default: patch
---

`qhhoj-judge check-config` runs the daemon's start checks (config, pinning, disk bound; the quota probed on a spare box) and exits non-zero when the daemon would refuse to start, so an install can check a config before switching to a release.
