// Fork bomb: the box allows one process; prints ok when fork fails.
#include <unistd.h>
#include <cstdio>
int main() { for (int i = 0; i < 64; i++) if (fork() < 0) { std::puts("ok"); return 0; } return 0; }
