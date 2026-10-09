// Input: a host TCP port and an abstract AF_UNIX name the suite listens on. Prints ok only when both
// connects fail; the suite also checks its listeners accepted nothing, so a runner that is merely
// offline cannot pass this for the wrong reason.
#include <arpa/inet.h>
#include <cstddef>
#include <cstdio>
#include <cstring>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
static bool tcp_reaches(int port) {
  int s = socket(AF_INET, SOCK_STREAM, 0);
  sockaddr_in a{}; a.sin_family = AF_INET; a.sin_port = htons(port); inet_pton(AF_INET, "127.0.0.1", &a.sin_addr);
  return s >= 0 && connect(s, (sockaddr*)&a, sizeof a) == 0;
}
static bool unix_reaches(const char* name) {
  int s = socket(AF_UNIX, SOCK_STREAM, 0);
  sockaddr_un a{}; a.sun_family = AF_UNIX; std::strncpy(a.sun_path + 1, name, sizeof a.sun_path - 2);
  socklen_t len = offsetof(sockaddr_un, sun_path) + 1 + std::strlen(name);
  return s >= 0 && connect(s, (sockaddr*)&a, len) == 0;
}
int main() {
  int port = 0; char name[100] = {0};
  if (std::scanf("%d %99s", &port, name) != 2) return 0;
  if (!tcp_reaches(port) && !unix_reaches(name)) std::puts("ok");
  return 0;
}
