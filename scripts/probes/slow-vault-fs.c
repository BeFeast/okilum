// Linux-only diagnostic preload: delay actual filesystem calls beneath the
// fixture vault. Cache files stay local. No product runtime configuration.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include <time.h>
#include <errno.h>

static _Atomic int vault_fd[65536];
static _Atomic unsigned long counts[7][4];
static _Atomic int phase;
static _Thread_local int resolving;
static const char *prefix;
static long delay_ns;
static long metadata_delay_ns;

__attribute__((constructor)) static void initialize(void) {
    prefix = getenv("OKILUM_SLOW_FS_PREFIX");
    const char *ms = getenv("OKILUM_SLOW_FS_MS");
    delay_ns = ms ? strtol(ms, NULL, 10) * 1000000L : 0;
    const char *metadata_us = getenv("OKILUM_SLOW_FS_METADATA_US");
    metadata_delay_ns = metadata_us ? strtol(metadata_us, NULL, 10) * 1000L : delay_ns;
}

void okilum_slow_fs_phase(const char *name) {
    atomic_store(&phase, !strcmp(name, "positive_control") ? 1 : !strcmp(name, "warm_primary") ? 2 : !strcmp(name, "complete_graph") ? 3 : !strcmp(name, "warm_reconcile") ? 4 : !strcmp(name, "serial_sources") ? 5 : !strcmp(name, "parallel_sources") ? 6 : 0);
}
unsigned long okilum_slow_fs_count(int operation) {
    return atomic_load(&counts[atomic_load(&phase)][operation]);
}
static int matches(const char *path) {
    return !resolving && path && prefix && !strncmp(path, prefix, strlen(prefix)) && strstr(path, "/vault");
}
static void pause_call(int operation) {
    int current_phase = atomic_load(&phase);
    if (!current_phase || !delay_ns || resolving) return;
    atomic_fetch_add(&counts[current_phase][operation], 1);
    long operation_delay_ns = operation >= 2 ? metadata_delay_ns : delay_ns;
    int saved = errno;
    struct timespec duration = {operation_delay_ns / 1000000000L, operation_delay_ns % 1000000000L};
    while (nanosleep(&duration, &duration) && errno == EINTR) {}
    errno = saved;
}
static void *symbol(const char *name) {
    resolving++;
    void *result = dlsym(RTLD_NEXT, name);
    resolving--;
    return result;
}
#define LOAD_REAL(name, result, arguments) \
    typedef result (*real_function) arguments; \
    static _Atomic(real_function) cached; \
    real_function real = atomic_load(&cached); \
    if (!real) { real = symbol(name); atomic_store(&cached, real); }
#define OPEN_FUNCTION(name) \
int name(const char *path, int flags, ...) { \
    LOAD_REAL(#name, int, (const char *, int, ...)); \
    mode_t mode = 0; \
    if ((flags & O_CREAT) || ((flags & O_TMPFILE) == O_TMPFILE)) { va_list args; va_start(args, flags); mode = va_arg(args, int); va_end(args); } \
    int tracked = matches(path) && !(flags & (O_WRONLY | O_RDWR)); \
    if (tracked) pause_call(0); \
    int fd = real(path, flags, mode); \
    if (fd >= 0 && fd < 65536) atomic_store(&vault_fd[fd], tracked); \
    return fd; \
}
OPEN_FUNCTION(open)
OPEN_FUNCTION(open64)
ssize_t read(int fd, void *buffer, size_t count) {
    LOAD_REAL("read", ssize_t, (int, void *, size_t));
    if (fd >= 0 && fd < 65536 && atomic_load(&vault_fd[fd])) pause_call(1);
    return real(fd, buffer, count);
}
int close(int fd) {
    LOAD_REAL("close", int, (int));
    if (fd >= 0 && fd < 65536) atomic_store(&vault_fd[fd], 0);
    return real(fd);
}
int statx(int fd, const char *path, int flags, unsigned int mask, struct statx *buffer) {
    LOAD_REAL("statx", int, (int, const char *, int, unsigned int, struct statx *));
    if (matches(path) || (fd >= 0 && fd < 65536 && atomic_load(&vault_fd[fd]))) pause_call(2);
    return real(fd, path, flags, mask, buffer);
}
#define STAT_FUNCTION(name) \
int name(const char *path, struct stat *buffer) { \
    LOAD_REAL(#name, int, (const char *, struct stat *)); \
    if (matches(path)) pause_call(2); \
    return real(path, buffer); \
}
STAT_FUNCTION(stat)
STAT_FUNCTION(lstat)
char *realpath(const char *path, char *resolved) {
    LOAD_REAL("realpath", char *, (const char *, char *));
    if (matches(path)) pause_call(3);
    return real(path, resolved);
}
__attribute__((destructor)) static void report(void) {
    const char *names[] = {"setup", "positive_control", "warm_primary", "complete_graph", "warm_reconcile", "serial_sources", "parallel_sources"};
    for (int p = 1; p < 7; p++) {
        fprintf(stderr, "SLOW_VAULT_FS phase=%s delay_ms=%ld metadata_delay_us=%ld opens=%lu reads=%lu stats=%lu canonicalize=%lu\n", names[p], delay_ns / 1000000L, metadata_delay_ns / 1000L,
                atomic_load(&counts[p][0]), atomic_load(&counts[p][1]), atomic_load(&counts[p][2]), atomic_load(&counts[p][3]));
    }
}
