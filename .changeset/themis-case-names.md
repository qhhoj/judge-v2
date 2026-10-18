---
default: patch
---

Themis checkers: the two directories on the checker's stdin end with `/`, as the old judge gave them, so `dir + "name.inp"` without a separator opens the right file. Each case's files are also laid out under the case's own input and answer file names, in every letter case and with any extension, and `.ans` literals in the checker source name the answer and output. Before this, a checker that names its files `name ".INP"` with no file I/O set was IE on every submission, and `dir + "polygon.inp"` or a `.ans` answer found no files. Extra names are hard links, not copies.
