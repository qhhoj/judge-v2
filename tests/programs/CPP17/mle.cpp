#include <cstdlib>
#include <cstring>
int main() { for (;;) { char* p = static_cast<char*>(std::malloc(1 << 20)); if (!p) return 0; std::memset(p, 1, 1 << 20); } }
