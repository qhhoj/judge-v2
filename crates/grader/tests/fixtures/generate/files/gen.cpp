// Prints "<2 * argv[1]> <argv[2]>"; "loop" never ends, "fail" exits 3 with a message on stderr.
#include "header.h"
#include <cstdio>
#include <cstdlib>
#include <cstring>

int main(int argc, char** argv) {
    if (argc > 1 && std::strcmp(argv[1], "loop") == 0) for (;;) {}
    if (argc > 1 && std::strcmp(argv[1], "fail") == 0) { std::fputs("FAIL bad arguments", stderr); return 3; }
    if (argc != 3) return 4;
    std::printf("%d %s\n", twice(std::atoi(argv[1])), argv[2]);
}
