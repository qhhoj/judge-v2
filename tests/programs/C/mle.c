#include <stdlib.h>
#include <string.h>
/* `last` keeps every block: clang -O2 deletes a malloc whose memory is never read, and the loop spins empty. */
char *volatile last;
int main(void) { for (;;) { char *p = malloc(1 << 20); if (!p) return 0; memset(p, 1, 1 << 20); last = p; } }
