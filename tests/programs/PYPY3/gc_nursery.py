import gc

# The nursery size the GC really uses, in bytes (PyPy only: CPython has no nursery).
print(gc.get_stats()._s.nursery_size)
