// wbox win32: termios helper stubs, dirent implementation, TUI stubs,
// small string/stdlib helpers. (sockets/epoll/tcgetattr/tcsetattr are
// real implementations in w32sock.c.)
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
#include "win32.h"

// ---------------------------------------------------------------- termios
// tcgetattr/tcsetattr (and console-mode application) live in w32sock.c
// (feat/net); the L1 stubs that used to sit here were deleted with the
// STUB_RENAMES build hack. The remaining helpers are the real symbols.

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

// ---------------------------------------------------------------- sockets
// The real socket/epoll/inet_pton/inet_ntop implementations live in
// w32sock.c (feat/net); the L1 ENOSYS stubs that used to be here were
// deleted together with the STUB_RENAMES build hack that hid them.

const struct in6_addr in6addr_any = {{0}};
const struct in6_addr in6addr_loopback = {{1}};

uint32_t inet_addr_(void);  // placate

// ---------------------------------------------------------------- dirent

struct __wbox_DIR {
  HANDLE h;
  WIN32_FIND_DATAW data;
  int first;
  long pos;  // entries returned so far; backs telldir()/d_off
  wchar_t wpath[W32_PATH_MAX];
  struct dirent ent;
};

// glibc's readdir skips getdents64 records whose d_ino is zero, so
// synthesize a stable non-zero inode from the file name (FNV-1a).
static ino_t WboxDirentIno(const char *name) {
  uint64_t h = 1469598103934665603ULL;
  for (const unsigned char *p = (const unsigned char *)name; *p; ++p) {
    h ^= *p;
    h *= 1099511628211ULL;
  }
  h &= 0x7fffffffffffffffULL;
  return (ino_t)(h ? h : 1);
}

static int WboxDirApiPath(const wchar_t *wpath,
                          wchar_t out[W32_PATH_MAX]) {
  DWORD n;
  size_t len;
  if (!wcsncmp(wpath, L"\\\\?\\", 4)) {
    if (wcslen(wpath) >= W32_PATH_MAX) return -1;
    wcscpy(out, wpath);
    return 0;
  }
  n = GetFullPathNameW(wpath, W32_PATH_MAX, out, NULL);
  if (!n || n >= W32_PATH_MAX) return -1;
  if (!(out[0] && out[1] == L':' && out[2] == L'\\')) return -1;
  len = wcslen(out);
  if (len + 4 >= W32_PATH_MAX) return -1;
  memmove(out + 4, out, (len + 1) * sizeof(*out));
  memcpy(out, L"\\\\?\\", 4 * sizeof(*out));
  return 0;
}

static DIR *OpendirW(const wchar_t *wpath, const char *path) {
  DIR *d = calloc(1, sizeof(DIR));
  if (!d) return NULL;
  if (WboxDirApiPath(wpath, d->wpath) ||
      wcslen(d->wpath) + 3 >= W32_PATH_MAX) {
    free(d);
    errno = ENAMETOOLONG;
    return NULL;
  }
  wcscat(d->wpath, L"\\*");
  (void)path;
  d->h = FindFirstFileW(d->wpath, &d->data);
  if (d->h == INVALID_HANDLE_VALUE) {
    free(d);
    errno = ENOENT;
    return NULL;
  }
  d->first = 1;
  return d;
}

DIR *opendir(const char *path) {
  wchar_t *wbuf = malloc(W32_PATH_MAX * sizeof(*wbuf));
  if (!wbuf) {
    errno = ENOMEM;
    return NULL;
  }
  if (MultiByteToWideChar(CP_UTF8, 0, path, -1, wbuf, W32_PATH_MAX) <= 0) {
    free(wbuf);
    errno = ENOENT;
    return NULL;
  }
  // strip trailing "\\*" not present here; OpendirW appends it
  DIR *d = OpendirW(wbuf, path);
  free(wbuf);
  return d;
}

DIR *fdopendir(int fd) {
  // AppContainer may deny GetFinalPathNameByHandleW even when the directory
  // itself is readable. Prefer the normalized path remembered by openat();
  // retain handle recovery for inherited/foreign descriptors.
  HANDLE h = (HANDLE)_get_osfhandle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return NULL;
  }
  wchar_t *wbuf = malloc(W32_PATH_MAX * sizeof(*wbuf));
  if (!wbuf) {
    errno = ENOMEM;
    return NULL;
  }
  extern int W32GetFdPath(int, wchar_t *, size_t);
  if (!W32GetFdPath(fd, wbuf, W32_PATH_MAX)) {
    DWORD n = GetFinalPathNameByHandleW(h, wbuf, W32_PATH_MAX, 0);
    if (!n || n >= W32_PATH_MAX) {
      free(wbuf);
      errno = ENOENT;
      return NULL;
    }
  }
  DIR *d = OpendirW(wbuf, NULL);
  free(wbuf);
  return d;
}

int closedir(DIR *d) {
  if (!d) return 0;
  FindClose(d->h);
  free(d);
  return 0;
}

// w32fd.c: reverse the %XXXX host-name escaping (F7/F8) so guests see
// their original UTF-8 / win32-illegal names.
void W32UnescapeName(wchar_t *w);

struct dirent *readdir(DIR *d) {
  if (!d) return NULL;
  if (!d->first) {
    if (!FindNextFileW(d->h, &d->data)) return NULL;
  }
  d->first = 0;
  W32UnescapeName(d->data.cFileName);
  WideCharToMultiByte(CP_UTF8, 0, d->data.cFileName, -1, d->ent.d_name,
                      sizeof(d->ent.d_name), NULL, NULL);
  d->ent.d_type = (d->data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY)
                      ? DT_DIR
                      : DT_REG;
  d->ent.d_ino = WboxDirentIno(d->ent.d_name);
  d->ent.d_off = ++d->pos;
  d->ent.d_reclen = sizeof(d->ent);
  return &d->ent;
}

void rewinddir(DIR *d) {
  if (!d) return;
  FindClose(d->h);
  d->h = FindFirstFileW(d->wpath, &d->data);
  d->first = 1;
  d->pos = 0;
}

void seekdir(DIR *d, long pos) {
  if (!d) return;
  if (pos < 0) return;
  rewinddir(d);
  while (d->pos < pos && readdir(d))
    ;
}

long telldir(DIR *d) { return d ? d->pos : -1; }
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

// realpath(3) for VFS (BLINK_PREFIX rootfs 解析): UTF-8 -> wide ->
// GetFullPathNameW -> UTF-8。Windows 不解析 symlink，L2 接受该差异。
char *realpath(const char *path, char *resolved) {
  wchar_t wsrc[520], wfull[520];
  if (!path || MultiByteToWideChar(CP_UTF8, 0, path, -1, wsrc, 520) <= 0) {
    errno = ENOENT;
    return NULL;
  }
  DWORD n = GetFullPathNameW(wsrc, 520, wfull, NULL);
  if (!n || n >= 520) {
    errno = ENOENT;
    return NULL;
  }
  char utf8[520 * 3];
  if (WideCharToMultiByte(CP_UTF8, 0, wfull, -1, utf8, sizeof(utf8), NULL,
                          NULL) <= 0) {
    errno = ENOENT;
    return NULL;
  }
  // blink 用返回串直接做路径前缀拼接，统一成 '/' 分隔
  for (char *p = utf8; *p; ++p)
    if (*p == '\\') *p = '/';
  if (resolved) {
    strcpy(resolved, utf8);
    return resolved;
  }
  char *out = malloc(strlen(utf8) + 1);
  if (out) strcpy(out, utf8);
  return out;
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
