// Prints a + b; crashes on a = 99.
#include <cstdio>

int main() {
    int a, b;
    if (std::scanf("%d %d", &a, &b) != 2) return 1;
    if (a == 99) *(volatile int*)0 = 1;
    std::printf("%d\n", a + b);
}
