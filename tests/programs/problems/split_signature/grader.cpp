#include <cstdio>
#include "sol.h"

long long bump(long long x) { return x + 1; }

int main() {
    long long a, b;
    if (scanf("%lld %lld", &a, &b) != 2) return 2;
    printf("%lld\n", add(a, b));
}
