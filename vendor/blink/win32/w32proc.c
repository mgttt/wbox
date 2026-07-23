// wbox win32: processes, ids, resources, time, misc
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <sys/sysinfo.h>
#include <sys/times.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include <windows.h>
#include <bcrypt.h>

// ---------------------------------------------------------------- ids

uid_t getuid(void) { return 1000; }
uid_t geteuid(void) { return 1000; }
gid_t getgid(void) { return 1000; }
gid_t getegid(void) { return 1000; }
int setuid(uid_t x) { return 0; }
int setgid(gid_t x) { return 0; }
int seteuid(uid_t x) { return 0; }
int setegid(gid_t x) { return 0; }
int setreuid(uid_t a, uid_t b) { return 0; }
int setregid(gid_t a, gid_t b) { return 0; }
int setresuid(uid_t a, uid_t b, uid_t c) { return 0; }
int setresgid(gid_t a, gid_t b, gid_t c) { return 0; }
int getresuid(uid_t *a, uid_t *b, uid_t *c) {
  *a = *b = *c = 1000;
  return 0;
}
int getresgid(gid_t *a, gid_t *b, gid_t *c) {
  *a = *b = *c = 1000;
  return 0;
}

pid_t getpid(void) { return (pid_t)GetCurrentProcessId(); }
pid_t getppid(void) { return 1; }
static pid_t g_pgrp = 1;
pid_t getpgrp(void) { return g_pgrp; }
pid_t getpgid(pid_t pid) { return g_pgrp; }
int setpgid(pid_t pid, pid_t pgid) {
  g_pgrp = pgid ? pgid : pid;
  return 0;
}
pid_t setsid(void) { return getpid(); }
pid_t getsid(pid_t pid) { return getpid(); }
pid_t tcgetpgrp(int fd) { return g_pgrp; }
int tcsetpgrp(int fd, pid_t pgrp) { return 0; }

// ---------------------------------------------------------------- fork/wait (L1: unsupported)

pid_t fork(void) {
  errno = ENOSYS;
  return -1;
}

pid_t vfork(void) {
  errno = ENOSYS;
  return -1;
}

pid_t wait(int *status) {
  errno = ECHILD;
  return -1;
}

pid_t waitpid(pid_t pid, int *status, int flags) {
  errno = ECHILD;
  return -1;
}

pid_t wait3(int *status, int flags, struct rusage *ru) {
  errno = ECHILD;
  return -1;
}

pid_t wait4(pid_t pid, int *status, int flags, struct rusage *ru) {
  errno = ECHILD;
  return -1;
}

int waitid(idtype_t t, id_t id, siginfo_t *si, int flags) {
  errno = ECHILD;
  return -1;
}

int execve(const char *path, char *const *argv, char *const *envp) {
  errno = ENOSYS;  // guest execve is rebuilt in-process by blink itself
  return -1;
}

int execv(const char *path, char *const *argv) {
  return execve(path, argv, NULL);
}

int execvp(const char *path, char *const *argv) {
  return execve(path, argv, NULL);
}

int fexecve(int fd, char *const *argv, char *const *envp) {
  errno = ENOSYS;
  return -1;
}

int daemon(int nochdir, int noclose) {
  errno = ENOSYS;
  return -1;
}

// ---------------------------------------------------------------- rlimit / usage

int getrlimit(int res, struct rlimit *rl) {
  switch (res) {
    case RLIMIT_NOFILE:
      rl->rlim_cur = 2048;
      rl->rlim_max = 4096;
      break;
    case RLIMIT_STACK:
      rl->rlim_cur = 8 << 20;
      rl->rlim_max = 8 << 20;
      break;
    default:
      rl->rlim_cur = RLIM_INFINITY;
      rl->rlim_max = RLIM_INFINITY;
      break;
  }
  return 0;
}

int setrlimit(int res, const struct rlimit *rl) {
  return 0;
}

int getrusage(int who, struct rusage *ru) {
  memset(ru, 0, sizeof(*ru));
  FILETIME ct, et, kt, ut;
  if (who == RUSAGE_SELF && GetProcessTimes(GetCurrentProcess(), &ct, &et,
                                            &kt, &ut)) {
    uint64_t k = ((uint64_t)kt.dwHighDateTime << 32) | kt.dwLowDateTime;
    uint64_t u = ((uint64_t)ut.dwHighDateTime << 32) | ut.dwLowDateTime;
    ru->ru_stime.tv_sec = k / 10000000;
    ru->ru_stime.tv_usec = (k % 10000000) / 10;
    ru->ru_utime.tv_sec = u / 10000000;
    ru->ru_utime.tv_usec = (u % 10000000) / 10;
  }
  return 0;
}

int getpriority(int which, int who) { return 0; }
int setpriority(int which, int who, int prio) { return 0; }
int nice(int inc) { return 0; }

// ---------------------------------------------------------------- sysinfo

int sysinfo(struct sysinfo *si) {
  MEMORYSTATUSEX ms;
  memset(si, 0, sizeof(*si));
  ms.dwLength = sizeof(ms);
  GlobalMemoryStatusEx(&ms);
  si->uptime = GetTickCount64() / 1000;
  si->totalram = ms.ullTotalPhys;
  si->freeram = ms.ullAvailPhys;
  si->totalswap = ms.ullTotalPageFile - ms.ullTotalPhys;
  si->freeswap = ms.ullAvailPageFile - ms.ullAvailPhys;
  si->procs = 64;
  si->mem_unit = 1;
  return 0;
}

int statvfs(const char *path, struct statvfs *sv) {
  memset(sv, 0, sizeof(*sv));
  ULARGE_INTEGER freeb, total, totalfree;
  if (GetDiskFreeSpaceExW(L"\\", &freeb, &total, &totalfree)) {
    sv->f_bsize = sv->f_frsize = 4096;
    sv->f_blocks = total.QuadPart / 4096;
    sv->f_bfree = totalfree.QuadPart / 4096;
    sv->f_bavail = freeb.QuadPart / 4096;
  } else {
    sv->f_bsize = sv->f_frsize = 4096;
    sv->f_blocks = sv->f_bfree = sv->f_bavail = 1 << 20;
  }
  sv->f_files = 1 << 20;
  sv->f_ffree = sv->f_favail = 1 << 19;
  sv->f_namemax = 255;
  return 0;
}

int fstatvfs(int fd, struct statvfs *sv) {
  return statvfs("/", sv);
}

clock_t times(struct tms *t) {
  struct rusage ru;
  getrusage(RUSAGE_SELF, &ru);
  t->tms_utime = ru.ru_utime.tv_sec * 100 + ru.ru_utime.tv_usec / 10000;
  t->tms_stime = ru.ru_stime.tv_sec * 100 + ru.ru_stime.tv_usec / 10000;
  t->tms_cutime = t->tms_cstime = 0;
  return GetTickCount64() / 10;
}

long sysconf(int name) {
  SYSTEM_INFO si;
  switch (name) {
    case _SC_PAGESIZE:
      return 4096;
    case _SC_NPROCESSORS_ONLN:
    case _SC_NPROCESSORS_CONF:
      GetSystemInfo(&si);
      return si.dwNumberOfProcessors;
    case _SC_CLK_TCK:
      return 100;
    case _SC_OPEN_MAX:
      return 2048;
    case _SC_PHYS_PAGES: {
      MEMORYSTATUSEX ms;
      ms.dwLength = sizeof(ms);
      GlobalMemoryStatusEx(&ms);
      return ms.ullTotalPhys / 4096;
    }
    case _SC_AVPHYS_PAGES: {
      MEMORYSTATUSEX ms;
      ms.dwLength = sizeof(ms);
      GlobalMemoryStatusEx(&ms);
      return ms.ullAvailPhys / 4096;
    }
    case _SC_HOST_NAME_MAX:
      return 64;
    case _SC_ARG_MAX:
      return 32768;
    case _SC_CHILD_MAX:
      return 256;
    default:
      return 4096;
  }
}

long pathconf(const char *path, int name) { return 255; }
long fpathconf(int fd, int name) { return 255; }
int getdtablesize(void) { return 2048; }
int getpagesize(void) { return 4096; }

// ---------------------------------------------------------------- time


int wbox_clock_gettime(int id, struct timespec *ts) {
  if (id == CLOCK_MONOTONIC || id == CLOCK_MONOTONIC_RAW ||
      id == CLOCK_BOOTTIME || id == CLOCK_MONOTONIC_COARSE) {
    static LARGE_INTEGER freq;
    LARGE_INTEGER c;
    if (!freq.QuadPart) QueryPerformanceFrequency(&freq);
    QueryPerformanceCounter(&c);
    ts->tv_sec = c.QuadPart / freq.QuadPart;
    ts->tv_nsec = (c.QuadPart % freq.QuadPart) * 1000000000LL / freq.QuadPart;
    return 0;
  }
  FILETIME ft;
  GetSystemTimePreciseAsFileTime(&ft);
  uint64_t t = ((uint64_t)ft.dwHighDateTime << 32) | ft.dwLowDateTime;
  ts->tv_sec = t / 10000000 - 11644473600LL;
  ts->tv_nsec = (t % 10000000) * 100;
  return 0;
}

int wbox_clock_settime(int id, const struct timespec *ts) {
  errno = EPERM;
  return -1;
}

int wbox_clock_getres(int id, struct timespec *ts) {
  (void)id;
  ts->tv_sec = 0;
  ts->tv_nsec = 100;  // GetSystemTimePreciseAsFileTime granularity
  return 0;
}

int nanosleep(const struct timespec *req, struct timespec *rem) {
  DWORD ms = req->tv_sec * 1000 + (req->tv_nsec + 999999) / 1000000;
  Sleep(ms ? ms : 1);
  if (rem) {
    rem->tv_sec = 0;
    rem->tv_nsec = 0;
  }
  return 0;
}

int wbox_clock_nanosleep(int id, int flags, const struct timespec *req,
                    struct timespec *rem) {
  return nanosleep(req, rem);
}

int usleep(unsigned usec) {
  Sleep((usec + 999) / 1000);
  return 0;
}

unsigned sleep(unsigned sec) {
  Sleep(sec * 1000);
  return 0;
}

unsigned alarm(unsigned sec) { return 0; }
int pause(void) {
  Sleep(INFINITE);
  return -1;
}

struct tm *gmtime_r(const time_t *t, struct tm *tm) {
  return gmtime_s(tm, t) == 0 ? tm : NULL;
}

struct tm *localtime_r(const time_t *t, struct tm *tm) {
  return localtime_s(tm, t) == 0 ? tm : NULL;
}

time_t timegm(struct tm *tm) { return _mkgmtime(tm); }

int timer_create(int id, struct sigevent *ev, timer_t *t) {
  errno = ENOSYS;
  return -1;
}

int timer_settime(timer_t t, int f, const struct itimerspec *n,
                  struct itimerspec *o) {
  errno = ENOSYS;
  return -1;
}

int timer_gettime(timer_t t, struct itimerspec *i) {
  errno = ENOSYS;
  return -1;
}

int timer_delete(timer_t t) {
  errno = ENOSYS;
  return -1;
}

int getitimer(int which, struct itimerval *v) {
  memset(v, 0, sizeof(*v));
  return 0;
}

int setitimer(int which, const struct itimerval *n, struct itimerval *o) {
  if (o) memset(o, 0, sizeof(*o));
  return 0;  // L1: no SIGALRM delivery
}

// ---------------------------------------------------------------- misc

ssize_t getrandom(void *buf, size_t n, unsigned flags) {
  NTSTATUS st = BCryptGenRandom(NULL, buf, n, BCRYPT_USE_SYSTEM_PREFERRED_RNG);
  if (st != 0) {
    errno = EIO;
    return -1;
  }
  return n;
}

unsigned long getauxval(unsigned long type) { return 0; }

int gethostname(char *name, size_t len) {
  wchar_t wbuf[64];
  DWORD n = 63;
  if (!GetComputerNameW(wbuf, &n)) {
    errno = EINVAL;
    return -1;
  }
  if (WideCharToMultiByte(CP_UTF8, 0, wbuf, -1, name, len, NULL, NULL) <= 0) {
    errno = ENAMETOOLONG;
    return -1;
  }
  return 0;
}

int sethostname(const char *name, size_t len) {
  errno = EPERM;
  return -1;
}

int getdomainname(char *name, size_t len) {
  if (len < 2) {
    errno = EINVAL;
    return -1;
  }
  strcpy(name, "");
  return 0;
}

int setdomainname(const char *name, size_t len) {
  errno = EPERM;
  return -1;
}

void sync(void) {}
int syncfs(int fd) { return 0; }

int getgroups(int n, gid_t *g) { return 0; }
int setgroups(size_t n, const gid_t *g) { return 0; }
int initgroups(const char *u, gid_t g) { return 0; }

int prctl(int op, ...) {
  errno = ENOSYS;
  return -1;
}

int sysctl(int *a, unsigned b, void *c, size_t *d, void *e, size_t f) {
  errno = ENOSYS;
  return -1;
}

int mount(const char *a, const char *b, const char *c, unsigned long d,
          const void *e) {
  errno = ENOSYS;
  return -1;
}

int umount(const char *a) {
  errno = ENOSYS;
  return -1;
}

int umount2(const char *a, int f) {
  errno = ENOSYS;
  return -1;
}

int sched_yield(void) {
  Sleep(0);
  return 0;
}

int fchown(int fd, uid_t u, gid_t g) { return 0; }
int chown(const char *p, uid_t u, gid_t g) { return 0; }
int lchown(const char *p, uid_t u, gid_t g) { return 0; }
int fchownat(int d, const char *p, uid_t u, gid_t g, int f) { return 0; }

int brk(void *p) {
  errno = ENOSYS;  // blink emulates brk itself; this shouldn't be called
  return -1;
}

void *sbrk(intptr_t n) {
  errno = ENOSYS;
  return (void *)-1;
}

int posix_memalign(void **p, size_t align, size_t size) {
  void *r = _aligned_malloc(size, align);
  if (!r) return ENOMEM;
  *p = r;
  return 0;
}

void *memalign(size_t align, size_t size) {
  return _aligned_malloc(size, align);
}

void *valloc(size_t size) { return _aligned_malloc(size, 4096); }

int mkostemp(char *tmpl, int flags) { return mkstemp(tmpl); }

int clearenv(void) {
  extern char **environ;
  if (environ) environ[0] = NULL;
  return 0;
}
