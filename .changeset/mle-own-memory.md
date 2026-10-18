---
default: patch
---

MLE without an OOM kill is the program's own memory over the limit, the smaller of the cgroup peak and the peak RSS, not the peak RSS alone. The peak RSS also counts mapped files already in page cache, a runtime's shared objects (~37 MiB of PyPy's): a PyPy a+b peaked at 56-69 MiB RSS (19-33 MiB charged), so PyPy on a 64 MiB problem was MLE.
