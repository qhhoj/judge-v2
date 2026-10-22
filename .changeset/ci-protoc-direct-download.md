---
default: patch
---

CI installs protoc 29.3 by a direct, sha256-pinned release download instead of arduino/setup-protoc, whose GitHub API lookup failed jobs intermittently. No runtime change.
