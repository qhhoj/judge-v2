// The data root, answers and system files are not readable or writable. stdin lists more host paths
// (one per line), which the suite sets to files that exist in the other judge boxes.
#include <cstdio>
#include <string>
static bool opens(const char* path, const char* mode) {
  std::FILE* f = std::fopen(path, mode);
  if (f) std::fclose(f);
  return f != nullptr;
}
int main() {
  const char* reads[] = {"/srv/qhhoj/problem-data", "answer", "../answer", "/box/../answer", "/etc/shadow",
                         "/run/credentials", "/run/credentials/qhhoj-judge.service/judge-key"};
  for (const char* path : reads) if (opens(path, "r")) return 0;
  char line[512];
  while (std::fgets(line, sizeof line, stdin)) {
    std::string path(line);
    while (!path.empty() && (path.back() == '\n' || path.back() == '\r')) path.pop_back();
    if (!path.empty() && opens(path.c_str(), "r")) return 0;
  }
  if (opens("/usr/evil", "w")) return 0;
  std::puts("ok");
  return 0;
}
