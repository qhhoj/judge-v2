---
default: patch
---

Boxes can be pinned by isolate cpuset lines (`qhhoj-judge isolate-cpus`) and given a per-box disk quota (`[isolate] disk_quota`, a headroom over the files each box is given and the output its run may write); both are off until the host is set up (DEPLOY.md), with a warning at start.
