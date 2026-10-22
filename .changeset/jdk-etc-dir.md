---
default: patch
---

JAVA, JAVA8 and KOTLIN bind the `/etc/java-NN-openjdk` directory of the JDK `/usr/lib/jvm/default-java` links to (`{jdk_etc}` in languages.toml) instead of a hard-coded JDK 21 path, so they work on hosts with another JDK major; a language whose bound directory is missing is not advertised, instead of retrying every job as a sandbox failure.
