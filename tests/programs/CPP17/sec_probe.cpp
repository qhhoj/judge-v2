// Escape attacks, each paired with a control. stdin: "box" or "host", the host pid of the test process, then
// host paths (one per line) that exist on the host and must not exist in the sandbox.
// "box": prints ok only when every attack failed with the expected errno, else the first failures.
// "host" (run by the suite outside the sandbox): prints ok only when every reachable thing was reachable, which
// proves the attacks use paths and calls that work when nothing isolates them.
#include <cerrno>
#include <csignal>
#include <cstdio>
#include <cstring>
#include <fcntl.h>
#include <pthread.h>
#include <string>
#include <sys/mount.h>
#include <sys/ptrace.h>
#include <sys/wait.h>
#include <unistd.h>
#include <vector>

static int open_errno(const std::string& path, int flags) {
  int fd = open(path.c_str(), flags, 0600);
  if (fd < 0) return errno;
  close(fd);
  return 0;
}
static bool one_of(int e, std::initializer_list<int> allowed) {
  for (int a : allowed) if (e == a) return true;
  return false;
}
static void* idle(void*) { usleep(300000); return nullptr; }
// Starts up to 200 threads; returns how many started and the pthread error that stopped it.
static int thread_bomb(int* error) {
  pthread_attr_t attr; pthread_attr_init(&attr); pthread_attr_setstacksize(&attr, 64 * 1024);
  pthread_t t; int started = 0; *error = 0;
  for (; started < 200; started++) if ((*error = pthread_create(&t, &attr, idle, nullptr)) != 0) break;
  return started;
}
static int ptrace_child() {  // host control: attaching to one's own child works
  pid_t child = fork();
  if (child == 0) { pause(); _exit(0); }
  int r = ptrace(PTRACE_ATTACH, child, 0, 0), e = errno;
  kill(child, SIGKILL); waitpid(child, nullptr, 0);
  return r < 0 ? e : 0;
}
static bool caps_dropped_and_not_root() {
  FILE* f = std::fopen("/proc/self/status", "r");
  if (!f) return false;
  char line[256]; bool saw = false, clean = true;
  while (std::fgets(line, sizeof line, f)) {
    if (!std::strncmp(line, "CapEff:", 7)) { saw = true; clean &= std::strstr(line, "0000000000000000") != nullptr; }
  }
  std::fclose(f);
  return saw && clean && getuid() != 0 && geteuid() != 0;
}

struct Report {
  std::string failures;
  void expect(const char* name, bool good, int e) {
    if (!good) failures += std::string(name) + "=" + std::to_string(e) + " ";
  }
};

static void box_attacks(Report& r, int host_pid, const std::vector<std::string>& paths) {
  r.expect("proc_of_host_process", open_errno("/proc/" + std::to_string(host_pid) + "/cmdline", O_RDONLY) == ENOENT, 0);
  r.expect("sys_visible", open_errno("/sys/kernel", O_RDONLY) == ENOENT, 0);
  int w = open_errno("/sys/kernel/uevent_helper", O_WRONLY);
  r.expect("sys_write", one_of(w, {ENOENT, EACCES, EROFS, EPERM}), w);
  w = open_errno("/proc/sys/kernel/core_pattern", O_WRONLY);
  r.expect("proc_sys_write", one_of(w, {EACCES, EROFS, EPERM, ENOENT}), w);
  w = open_errno("/usr/evil", O_WRONLY | O_CREAT);
  r.expect("write_usr", one_of(w, {EACCES, EROFS, EPERM}), w);
  for (const auto& p : paths) { int e = open_errno(p, O_RDONLY); r.expect(p.c_str(), e == ENOENT, e); }
  r.expect("setuid_root", setuid(0) < 0 && errno == EPERM, errno);
  r.expect("setgid_root", setgid(0) < 0 && errno == EPERM, errno);
  r.expect("setuid_self_works", setuid(getuid()) == 0, errno);  // control: the call is not simply filtered
  r.expect("unprivileged", caps_dropped_and_not_root(), 0);
  int e = ptrace(PTRACE_ATTACH, 1, 0, 0) < 0 ? errno : 0;
  r.expect("ptrace_pid1", e == EPERM, e);
  e = ptrace(PTRACE_ATTACH, getppid(), 0, 0) < 0 ? errno : 0;
  r.expect("ptrace_parent", e == EPERM, e);
  e = mount("none", "/tmp", "tmpfs", 0, nullptr) < 0 ? errno : 0;
  r.expect("mount", e == EPERM, e);
  e = chroot("/proc") < 0 ? errno : 0;
  r.expect("chroot", e == EPERM, e);
  int err; int started = thread_bomb(&err);
  r.expect("thread_bomb", started < 8 && err == EAGAIN, started);
}

static void host_controls(Report& r, int host_pid, const std::vector<std::string>& paths) {
  r.expect("proc_of_host_process", open_errno("/proc/" + std::to_string(host_pid) + "/cmdline", O_RDONLY) == 0, 0);
  r.expect("sys_visible", open_errno("/sys/kernel", O_RDONLY) == 0, 0);
  for (const auto& p : paths) { int e = open_errno(p, O_RDONLY); r.expect(p.c_str(), e == 0, e); }
  r.expect("setuid_self_works", setuid(getuid()) == 0, errno);
  int e = ptrace_child();
  r.expect("ptrace_child", e == 0, e);
  int err; int started = thread_bomb(&err);
  r.expect("thread_bomb_possible", started == 200, started);
}

int main() {
  char mode[16] = {0}; int host_pid = 0;
  if (std::scanf("%15s %d", mode, &host_pid) != 2) return 0;
  std::vector<std::string> paths; char line[1024];
  while (std::scanf(" %1023[^\n]", line) == 1) paths.push_back(line);
  Report r;
  if (!std::strcmp(mode, "host")) host_controls(r, host_pid, paths); else box_attacks(r, host_pid, paths);
  std::puts(r.failures.empty() ? "ok" : r.failures.c_str());
  return 0;
}
