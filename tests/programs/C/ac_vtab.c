#include <stdio.h>
/* The sum with a vertical tab (\v) before and after it: the standard checker splits tokens on \v. */
int main(void) { long long a, b; scanf("%lld %lld", &a, &b); printf("\v%lld\v\n", a + b); return 0; }
