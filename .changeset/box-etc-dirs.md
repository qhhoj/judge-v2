---
default: patch
---

PAS, JAVA, JAVA8 and KOTLIN work on Ubuntu 24.04: their runtimes reach `/etc/alternatives` and `/etc/java-21-openjdk` through symlinks, so languages.toml binds exactly those directories read-only into the box.
