// wbox win32: termios console stubs, sockets/epoll stubs (L1 no network),
// dirent implementation, TUI stubs, small string/stdlib helpers.
#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <termios.h>
#include <unistd.h>

intptr_t _get_osfhandle(int);
#include <windows.h>

#include "blink/pty.h"
#include "blink/machine.h"

// ---------------------------------------------------------------- termios
// L1: dumb console. tcgetattr returns a sane cooked-mode profile;
// tcsetattr(ICANON off) flips console to raw-ish input so shells work.

static struct termios g_tios = {
    .c_iflag = ICRNL | IXON,
    .c_oflag = OPOST | ONLCR,
    .c_cflag = CS8 | CREAD | HUPCL,
    .c_lflag = ISIG | ICANON | ECHO | ECHOE | ECHOK | IEXTEN,
    .c_line = 0,
    .c_cc = {3, 28, 127, 21, 4, 0, 1, 0, 17, 19, 26, 0, 18, 15, 23, 22, 0},
    .c_ispeed = B38400,
    .c_ospeed = B38400,
};

static void W32ApplyConsole(int fd, const struct termios *t) {
  HANDLE h = (HANDLE)_get_osfhandle(fd);
  if (h == INVALID_HANDLE_VALUE) return;
  DWORD m;
  if (!GetConsoleMode(h, &m)) return;
  if (t->c_lflag & ICANON) {
    m |= ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT;
  } else {
    m &= ~(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT);
    m |= ENABLE_VIRTUAL_TERMINAL_INPUT;
  }
  SetConsoleMode(h, m);
  // try to enable VT output for colors/readline
  HANDLE ho = GetStdHandle(STD_OUTPUT_HANDLE);
  DWORD om;
  if (GetConsoleMode(ho, &om)) {
    SetConsoleMode(ho, om | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
  }
}

int tcgetattr(int fd, struct termios *t) {
  *t = g_tios;
  return 0;
}

int tcsetattr(int fd, int act, const struct termios *t) {
  g_tios = *t;
  W32ApplyConsole(fd, t);
  return 0;
}

int tcdrain(int fd) { return 0; }
int tcflow(int fd, int act) { return 0; }
int tcflush(int fd, int q) { return 0; }
int tcsendbreak(int fd, int d) { return 0; }
speed_t cfgetispeed(const struct termios *t) { return t->c_ispeed; }
speed_t cfgetospeed(const struct termios *t) { return t->c_ospeed; }
int cfsetispeed(struct termios *t, speed_t s) {
  t->c_ispeed = s;
  return 0;
}
int cfsetospeed(struct termios *t, speed_t s) {
  t->c_ospeed = s;
  return 0;
}
void cfmakeraw(struct termios *t) {
  t->c_iflag &= ~(IGNBRK | BRKINT | PARMRK | ISTRIP | INLCR | IGNCR | ICRNL |
                  IXON);
  t->c_oflag &= ~OPOST;
  t->c_lflag &= ~(ECHO | ECHONL | ICANON | ISIG | IEXTEN);
  t->c_cflag &= ~(CSIZE | PARENB);
  t->c_cflag |= CS8;
  t->c_cc[VMIN] = 1;
  t->c_cc[VTIME] = 0;
}
pid_t tcgetsid(int fd) { return getpid(); }

// ---------------------------------------------------------------- sockets (L1: off)

static int Nosock(void) {
  errno = EPROTONOSUPPORT;
  return -1;
}

int socket(int d, int t, int p) { return Nosock(); }
int socketpair(int d, int t, int p, int f[2]) { return Nosock(); }
int bind(int s, const struct sockaddr *a, socklen_t l) { return Nosock(); }
int connect(int s, const struct sockaddr *a, socklen_t l) { return Nosock(); }
int listen(int s, int b) { return Nosock(); }
int accept(int s, struct sockaddr *a, socklen_t *l) { return Nosock(); }
int accept4(int s, struct sockaddr *a, socklen_t *l, int f) { return Nosock(); }
int shutdown(int s, int h) { return Nosock(); }
ssize_t send(int s, const void *b, size_t n, int f) { return Nosock(); }
ssize_t recv(int s, void *b, size_t n, int f) { return Nosock(); }
ssize_t sendto(int s, const void *b, size_t n, int f, const struct sockaddr *a,
               socklen_t l) { return Nosock(); }
ssize_t recvfrom(int s, void *b, size_t n, int f, struct sockaddr *a,
                 socklen_t *l) { return Nosock(); }
ssize_t sendmsg(int s, const struct msghdr *m, int f) { return Nosock(); }
ssize_t recvmsg(int s, struct msghdr *m, int f) { return Nosock(); }
int getsockopt(int s, int l, int o, void *v, socklen_t *n) { return Nosock(); }
int setsockopt(int s, int l, int o, const void *v, socklen_t n) { return Nosock(); }
int getsockname(int s, struct sockaddr *a, socklen_t *l) { return Nosock(); }
int getpeername(int s, struct sockaddr *a, socklen_t *l) { return Nosock(); }
int sockatmark(int s) { return Nosock(); }

const struct in6_addr in6addr_any = {{0}};
const struct in6_addr in6addr_loopback = {{1}};

uint32_t inet_addr_(void);  // placate
int inet_pton(int af, const char *src, void *dst) {
  errno = EAFNOSUPPORT;
  return -1;
}
const char *inet_ntop(int af, const void *src, char *dst, socklen_t size) {
  errno = EAFNOSUPPORT;
  return NULL;
}

// ---------------------------------------------------------------- epoll (L1: stubs)

int epoll_create(int size) {
  errno = ENOSYS;
  return -1;
}
int epoll_create1(int flags) {
  errno = ENOSYS;
  return -1;
}
int epoll_ctl(int epfd, int op, int fd, struct epoll_event *ev) {
  errno = ENOSYS;
  return -1;
}
int epoll_wait(int epfd, struct epoll_event *ev, int max, int timeout) {
  errno = ENOSYS;
  return -1;
}
int epoll_pwait(int epfd, struct epoll_event *ev, int max, int timeout,
                const sigset_t *mask) {
  errno = ENOSYS;
  return -1;
}

// ---------------------------------------------------------------- dirent

struct __wbox_DIR {
  HANDLE h;
  WIN32_FIND_DATAW data;
  int first;
  wchar_t wpath[520];
  struct dirent ent;
};

static DIR *OpendirW(const wchar_t *wpath, const char *path) {
  DIR *d = calloc(1, sizeof(DIR));
  if (!d) return NULL;
  wchar_t wbuf[520];
  if (wcslen(wpath) + 3 >= 520) {
    free(d);
    errno = ENAMETOOLONG;
    return NULL;
  }
  wcscpy(wbuf, wpath);
  wcscat(wbuf, L"\\*");
  wcscpy(d->wpath, wbuf);
  (void)path;
  d->h = FindFirstFileW(wbuf, &d->data);
  if (d->h == INVALID_HANDLE_VALUE) {
    free(d);
    errno = ENOENT;
    return NULL;
  }
  d->first = 1;
  return d;
}

DIR *opendir(const char *path) {
  wchar_t wbuf[520];
  if (MultiByteToWideChar(CP_UTF8, 0, path, -1, wbuf, 520) <= 0) {
    errno = ENOENT;
    return NULL;
  }
  // strip trailing "\\*" not present here; OpendirW appends it
  return OpendirW(wbuf, path);
}

DIR *fdopendir(int fd) {
  // blink's getdents64 opens the dir with open(O_DIRECTORY) and then
  // fdopendir()s the fd; recover the path from the handle.
  HANDLE h = (HANDLE)_get_osfhandle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return NULL;
  }
  wchar_t wbuf[520];
  DWORD n = GetFinalPathNameByHandleW(h, wbuf, 520, 0);
  if (!n || n >= 520) {
    errno = ENOENT;
    return NULL;
  }
  // wine returns "\\?\Z:\path"; strip the "\\?\" prefix
  wchar_t *p = wbuf;
  if (!wcsncmp(p, L"\\\\?\\", 4)) p += 4;
  return OpendirW(p, NULL);
}

int closedir(DIR *d) {
  if (!d) return 0;
  FindClose(d->h);
  free(d);
  return 0;
}

struct dirent *readdir(DIR *d) {
  if (!d) return NULL;
  if (!d->first) {
    if (!FindNextFileW(d->h, &d->data)) return NULL;
  }
  d->first = 0;
  WideCharToMultiByte(CP_UTF8, 0, d->data.cFileName, -1, d->ent.d_name,
                      sizeof(d->ent.d_name), NULL, NULL);
  d->ent.d_type = (d->data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY)
                      ? DT_DIR
                      : DT_REG;
  d->ent.d_ino = 0;
  d->ent.d_reclen = sizeof(d->ent);
  return &d->ent;
}

void rewinddir(DIR *d) {
  if (!d) return;
  FindClose(d->h);
  d->h = FindFirstFileW(d->wpath, &d->data);
  d->first = 1;
}

void seekdir(DIR *d, long pos) {}
long telldir(DIR *d) { return 0; }
int dirfd(DIR *d) { return -1; }

int alphasort(const struct dirent **a, const struct dirent **b) {
  return strcoll((*a)->d_name, (*b)->d_name);
}

int scandir(const char *path, struct dirent ***out,
            int (*filter)(const struct dirent *),
            int (*sort)(const struct dirent **, const struct dirent **)) {
  DIR *d = opendir(path);
  if (!d) return -1;
  int n = 0, cap = 16;
  struct dirent **list = malloc(cap * sizeof(*list));
  struct dirent *e;
  while ((e = readdir(d))) {
    if (filter && !filter(e)) continue;
    if (n == cap) {
      cap *= 2;
      list = realloc(list, cap * sizeof(*list));
    }
    list[n] = malloc(sizeof(struct dirent));
    *list[n] = *e;
    ++n;
  }
  closedir(d);
  if (sort) qsort(list, n, sizeof(*list), (void *)sort);
  *out = list;
  return n;
}

// ---------------------------------------------------------------- strings

char *stpcpy(char *dst, const char *src) {
  while ((*dst = *src++)) ++dst;
  return dst;
}

char *stpncpy(char *dst, const char *src, size_t n) {
  size_t l = strnlen(src, n);
  memcpy(dst, src, l);
  memset(dst + l, 0, n - l);
  return dst + l;
}

char *strchrnul(const char *s, int c) {
  while (*s && *s != (char)c) ++s;
  return (char *)s;
}

void *memccpy(void *dst, const void *src, int c, size_t n) {
  unsigned char *d = dst;
  const unsigned char *s = src;
  while (n--) {
    *d++ = *s;
    if (*s++ == (unsigned char)c) return d;
  }
  return NULL;
}

void *mempcpy(void *dst, const void *src, size_t n) {
  memcpy(dst, src, n);
  return (char *)dst + n;
}

void *rawmemchr(const void *s, int c) {
  const unsigned char *p = s;
  while (*p != (unsigned char)c) ++p;
  return (void *)p;
}

void *memrchr(const void *s, int c, size_t n) {
  const unsigned char *p = (const unsigned char *)s + n;
  while (n--) {
    if (*--p == (unsigned char)c) return (void *)p;
  }
  return NULL;
}

int strverscmp(const char *a, const char *b) { return strcmp(a, b); }

void explicit_bzero(void *p, size_t n) {
  volatile unsigned char *v = p;
  while (n--) *v++ = 0;
}

int dprintf(int fd, const char *fmt, ...) {
  char buf[4096];
  va_list ap;
  va_start(ap, fmt);
  int n = vsnprintf(buf, sizeof(buf), fmt, ap);
  va_end(ap);
  if (n < 0) return n;
  return write(fd, buf, n) == n ? n : -1;
}

int vdprintf(int fd, const char *fmt, va_list ap) {
  char buf[4096];
  int n = vsnprintf(buf, sizeof(buf), fmt, ap);
  if (n < 0) return n;
  return write(fd, buf, n) == n ? n : -1;
}

int asprintf(char **out, const char *fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  int n = vsnprintf(NULL, 0, fmt, ap);
  va_end(ap);
  if (n < 0) return n;
  *out = malloc(n + 1);
  va_start(ap, fmt);
  vsnprintf(*out, n + 1, fmt, ap);
  va_end(ap);
  return n;
}

int vasprintf(char **out, const char *fmt, va_list ap) {
  va_list ap2;
  va_copy(ap2, ap);
  int n = vsnprintf(NULL, 0, fmt, ap2);
  va_end(ap2);
  if (n < 0) return n;
  *out = malloc(n + 1);
  return vsnprintf(*out, n + 1, fmt, ap);
}

char *strndup(const char *s, size_t n) {
  size_t l = strnlen(s, n);
  char *r = malloc(l + 1);
  memcpy(r, s, l);
  r[l] = 0;
  return r;
}

char *strptime(const char *s, const char *fmt, struct tm *tm) {
  errno = ENOSYS;
  return NULL;
}

char *tmpnam_r(char *buf) {
  return tmpnam(buf);
}

char *asctime_r(const struct tm *tm, char *buf) {
  strcpy(buf, "Thu Jan  1 00:00:00 1970\n");
  return buf;
}

char *ctime_r(const time_t *t, char *buf) {
  struct tm tm;
  if (!gmtime_r(t, &tm)) return NULL;
  return asctime_r(&tm, buf);
}

// ---------------------------------------------------------------- TUI stubs (real mode only; unused by L1)

struct Pty *pty;
struct Machine *m;
bool ptyisenabled;
int vidya;
int ttyin;

void SetCarry(bool cf) {}
void DrawDisplayOnly(void) {}
void ReactiveDraw(void) {}
void Redraw(bool force) {}
long HasPendingKeyboard(void) { return 0; }
void HandleAppReadInterrupt(void) {}
int ReadAnsi(int fd, char *buf, int size) { return -1; }

