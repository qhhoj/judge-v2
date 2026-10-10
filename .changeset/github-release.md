---
default: patch
---

Pushing a `v*` tag publishes a GitHub Release with the tarball and its sha256; DEPLOY.md downloads it with `gh release download`.
