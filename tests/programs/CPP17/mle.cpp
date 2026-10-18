#include <cstdlib>
#include <cstring>
// `last` keeps every block: clang++ -O2 deletes a malloc whose memory is never read, then the empty loop and main's body.
char* volatile last;
int main() { for (;;) { char* p = static_cast<char*>(std::malloc(1 << 20)); if (!p) return 0; std::memset(p, 1, 1 << 20); last = p; } }
