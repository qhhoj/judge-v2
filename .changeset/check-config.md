---
default: patch
---

`qhhoj-judge check-config` runs the daemon's config, pinning and disk-bound checks (the quota probed on a spare box) and exits non-zero when one fails, so an install can check a config before switching to a release.
