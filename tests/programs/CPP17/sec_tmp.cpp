// Case 1 leaves a file in /tmp and in /box; case 2 must see neither.
#include <cstdio>
int main() {
  int n = 0;
  if (std::scanf("%d", &n) != 1) return 0;
  if (n == 1) {
    if (std::FILE* f = std::fopen("/tmp/qhhoj-mark", "w")) std::fclose(f);
    if (std::FILE* f = std::fopen("mark", "w")) std::fclose(f);
    std::puts("ok");
    return 0;
  }
  std::FILE* a = std::fopen("/tmp/qhhoj-mark", "r");
  std::FILE* b = std::fopen("mark", "r");
  std::puts(a || b ? "leak" : "ok");
  return 0;
}
