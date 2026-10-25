---
default: patch
---

A JVM run no longer reports the JDK's page cache as its own memory: JAVA, JAVA8 and KOTLIN `preload` the JDK's `lib` (read into the page cache by the judge before each compile and run), so a Java run reports the same memory whether or not the JDK was in the page cache before it (it reported 46 MiB more for a hello world when it was not).
