---
default: patch
---

Hello.build is the version plus the commit (`2.3.9+<12 hex>`) when the build knows it (the image's `REVISION`, `build-release.sh`), else the version alone, so the bridge can tell two builds of one release apart.
