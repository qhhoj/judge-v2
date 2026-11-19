---
default: patch
---

`check-host.sh` accepts problem data on a local filesystem when the judge user can read but not write it (a judge on the site host); NFS mounts keep the flag and `actimeo` checks.
