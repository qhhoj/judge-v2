---
default: patch
---

Checkers and interactors: testlib `points N` (and a default checker's exit 2 share) with N of 0 or less is a wrong answer. It was AC with 0 points: counted as accepted, and it did not stop a batch, a short-circuit run, or a full run at a failed pretest. An interactor's `points N` is N percent of the case when `interactive` sets `treat_checker_points_as_percentage: true` (optional, default false; feature `interactive.percentage`); before, it was always N points of the case.
