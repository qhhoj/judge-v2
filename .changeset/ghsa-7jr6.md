---
default: patch
---

Security update (GHSA-7jr6-jv73-f5hm). Deploy note: the systemd unit now needs `LimitMEMLOCK=infinity` (the packaged unit sets it); install the new unit and run `systemctl daemon-reload` before restarting the daemon.
