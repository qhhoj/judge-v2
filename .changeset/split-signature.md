---
default: minor
---

#### Split signature grades the grader and the contestant in separate sandboxes

`signature_grader.split: true` runs the author's entry and the contestant's functions in two isolate boxes
joined by pipes, with generated C++ glue. The contestant is charged its own CPU plus the CPU of the callbacks
it calls. The judge advertises `signature.split`. Problems without the key keep the one-process path unchanged.
