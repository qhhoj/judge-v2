---
default: patch
---

Test that a JVM in a box pinned to one core sees one processor, so JAVA, JAVA8 and KOTLIN size their JIT threads the same on every host; `languages.toml` notes why no `-XX:ActiveProcessorCount` is needed.
