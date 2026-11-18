#include "sol.h"
// 20000 round trips to the grader, each result feeding the next call, then the honest sum.
long long add(long long a, long long b) {
    long long x = 0;
    for (int i = 0; i < 20000; i++) x = bump(x);
    return a + b + (x - 20000);
}
