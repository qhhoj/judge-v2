---
default: patch
---

A Java source with a `package` line, or with a class name too long for its `<class>.java` file name (over 255 bytes), is a compile error with a message saying why, instead of a run-time failure on every case or a sandbox-failure retry ending in IE.
