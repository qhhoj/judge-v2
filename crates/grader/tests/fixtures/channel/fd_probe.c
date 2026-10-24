/* Run in both boxes of a split session. Prints one `name result` line per check: which fds this process holds
 * and what it can do with them. The test decides what is expected. Generic: no problem content. */
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static const char *result(int status) { return status == 0 ? "ok" : strerrorname_np(errno); }

static void open_fds(void) {
    DIR *dir = opendir("/proc/self/fd");
    int listing = dirfd(dir), fds[64], count = 0;
    for (struct dirent *e; (e = readdir(dir)) != NULL;) {
        int fd = atoi(e->d_name);
        if (e->d_name[0] != '.' && fd != listing && count < 64) fds[count++] = fd;
    }
    closedir(dir);
    for (int i = 1; i < count; i++)
        for (int j = i; j > 0 && fds[j - 1] > fds[j]; j--) { int t = fds[j]; fds[j] = fds[j - 1]; fds[j - 1] = t; }
    printf("fds");
    for (int i = 0; i < count; i++) printf(" %d", fds[i]);
    printf("\n");
}

static void shared_memory(void) {
    char target[256] = {0};
    if (readlink("/proc/self/fd/5", target, sizeof target - 1) < 0) strcpy(target, "unreadable");
    printf("fd5_target %s\n", strncmp(target, "/memfd:", 7) == 0 ? "memfd" : target);
    printf("fd5_mode %s\n", (fcntl(5, F_GETFL) & O_ACCMODE) == O_RDWR ? "rdwr" : "other");
    struct stat st = {0};
    fstat(5, &st);
    printf("fd5_size %lld\n", (long long)st.st_size);
    printf("reopen %s\n", open("/proc/self/fd/5", O_RDWR) >= 0 ? "ok" : strerrorname_np(errno));
    printf("grow %s\n", result(ftruncate(5, st.st_size * 2)));
    printf("shrink %s\n", result(ftruncate(5, 4096)));
    printf("add_seals %s\n", result(fcntl(5, F_ADD_SEALS, F_SEAL_WRITE)));
    printf("fchmod %s\n", result(fchmod(5, 0666)));
    printf("fallocate_grow %s\n", result(fallocate(5, 0, st.st_size, 4096)));
}

/* What fd points at, as /proc shows it: `pipe:[inode]` for a doorbell, so two boxes can be compared. */
static void print_target(int fd) {
    char path[32], target[256] = {0};
    snprintf(path, sizeof path, "/proc/self/fd/%d", fd);
    if (readlink(path, target, sizeof target - 1) < 0) strcpy(target, "unreadable");
    printf("fd%d_target %s\n", fd, target);
}

static void doorbells(void) {
    print_target(3);
    print_target(4);
    printf("fd4_nonblock %s\n", (fcntl(4, F_GETFL) & O_NONBLOCK) ? "yes" : "no");
    printf("fd3_nonblock %s\n", (fcntl(3, F_GETFL) & O_NONBLOCK) ? "yes" : "no");
}

int main(void) {
    open_fds();
    shared_memory();
    doorbells();
    fflush(stdout);
    pid_t child = fork();
    if (child == 0) _exit(0);
    printf("fork %s\n", child > 0 ? "ok" : strerrorname_np(errno));
    printf("session_page %s\n", access(".qhhoj-session", R_OK | W_OK) == 0 ? "rw" : "absent");
    return 0;
}
