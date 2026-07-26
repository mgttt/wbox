// wbox win32: POSIX fd layer over Win32 HANDLEs + CRT fd numbers.
// guest fd == CRT fd (pass-through model like blink's hostfs).
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/ioctl.h>
#include <poll.h>
#include <sys/select.h>
#include <signal.h>
#include <unistd.h>

#include <windows.h>
#include <bcrypt.h>
// CRT interop (declared manually to avoid io.h conflicts)
int _open_osfhandle(intptr_t, int);
intptr_t _get_osfhandle(int);
int _close(int);
int _dup2(int, int);

#include "win32.h"

#ifndef O_ACCMODE
#define O_ACCMODE 3
#endif

// ---------------------------------------------------------------- paths

// translate guest (linux) path to win32 wide path.
// - /dev/null -> NUL, /dev/tty -> CONIN$/CONOUT$ handled by caller
// - absolute "/x/y" is rooted at the WBOX_ROOT drive (default Z: which
//   is wine's unix-filesystem drive; on real Windows set WBOX_ROOT to
//   e.g. "C:\wbox" to jail the guest fs).
// C2 (security-audit): textual path normalization. Resolves "." and ".."
// components WITHOUT touching the host filesystem; a ".." that would pop
// above `floor` path components (the jail root) is rejected with -1.
// `in` must be absolute (drive-letter prefixed) or relative; the drive
// prefix ("X:") is preserved verbatim and counts as 0 components.
static int W32NormalizeW(const wchar_t *in, wchar_t *out, int floor) {
  wchar_t tmp[MAX_PATH];
  wchar_t *comps[MAX_PATH / 2];
  int n = 0;
  const wchar_t *s = in;
  wchar_t *o = out, *t;
  if (in[0] && in[1] == L':') {
    out[0] = in[0];
    out[1] = in[1];
    o = out + 2;
    s = in + 2;
  } else if (in[0] == L'\\') {
    return -1;  // UNC / device paths are never legitimate here
  }
  if (wcslen(s) >= MAX_PATH) return -1;
  wcscpy(tmp, s);
  for (t = tmp;;) {
    wchar_t *e, save;
    while (*t == L'\\' || *t == L'/') ++t;
    if (!*t) break;
    e = t;
    while (*e && *e != L'\\' && *e != L'/') ++e;
    save = *e;
    *e = 0;
    if (!wcscmp(t, L".")) {
      // skip
    } else if (!wcscmp(t, L"..")) {
      if (n <= floor) return -1;  // escape above the jail root
      --n;
    } else {
      comps[n++] = t;
    }
    if (!save) break;
    t = e + 1;
  }
  for (int i = 0; i < n; ++i) {
    size_t l = wcslen(comps[i]);
    if (i || o != out) *o++ = L'\\';
    memcpy(o, comps[i], l * sizeof(wchar_t));
    o += l;
  }
  if (o == out + 2 && out[1] == L':') {
    *o++ = L'\\';  // "Z:" -> "Z:\"
  } else if (o == out) {
    *o++ = L'.';  // "." or empty relative path stays "."
  }
  *o = 0;
  return 0;
}

// number of path components in the WBOX_ROOT prefix (the jail floor for
// absolute guest paths): "Z:" -> 0, "C:\wbox" -> 1, etc.
static int W32RootComps(const wchar_t *root) {
  int n = 0;
  const wchar_t *s = root;
  if (s[0] && s[1] == L':') s += 2;
  while (*s) {
    while (*s == L'\\' || *s == L'/') ++s;
    if (!*s) break;
    ++n;
    while (*s && *s != L'\\' && *s != L'/') ++s;
  }
  return n;
}

// C2 (security-audit): THE jail root. Every host path that reaches a
// Win32 API must textually resolve inside this directory — this is the
// single egress check shared by all path entries (W32Path for cwd-based
// and absolute paths, W32ResolveAt for the dirfd-anchored *at family).
// It is the 雏形 of the planned unified path-normalization entry point:
// VfsInit publishes the VFS root via WBOX_ROOT (BLINK_PREFIX, or the
// launch cwd under jail-by-default), so the VFS layer and this fd layer
// never disagree about what "outside the sandbox" means.
static const wchar_t *W32JailRoot(int *comps) {
  static wchar_t root[MAX_PATH];
  static int root_init, root_comps;
  if (!root_init) {
    char *r = getenv("WBOX_ROOT");
    if (!r || !*r) {
      // jail-by-default fallback (VfsInit not run yet): the launch cwd
      DWORD m = GetCurrentDirectoryW(MAX_PATH, root);
      if (!m || m >= MAX_PATH) {
        wcscpy(root, L"Z:\\");  // last resort: wine unix fs root
      }
    } else {
      root[0] = 0;
      MultiByteToWideChar(CP_UTF8, 0, r, -1, root, MAX_PATH - 1);
      root[MAX_PATH - 1] = 0;
    }
    for (int n = 0; root[n]; ++n)
      if (root[n] == L'/') root[n] = L'\\';
    // strip trailing backslashes ("Z:\" keeps its root slash)
    size_t l = wcslen(root);
    while (l > 3 && root[l - 1] == L'\\') root[--l] = 0;
    root_comps = W32RootComps(root);
    root_init = 1;
    if (getenv("WBOX_DEBUG_PATH"))
      fprintf(stderr, "[w32jailroot] root='%S' comps=%d env='%s'\n", root,
              root_comps, r ? r : "(null)");
  }
  if (comps) *comps = root_comps;
  return root;
}

// C2: case-insensitive check that the NORMALIZED ABSOLUTE path `abs`
// ("X:\..." drive-prefixed) resolves inside the jail root.
static int W32WithinRoot(const wchar_t *abs) {
  const wchar_t *root = W32JailRoot(NULL);
  size_t rl = wcslen(root);
  if (rl >= 2 && root[1] == L':' && rl == 2) {
    // bare drive root "Z:" — everything on the drive is inside
    return !_wcsnicmp(abs, root, 2) ? 0 : -1;
  }
  if (_wcsnicmp(abs, root, rl)) return -1;
  wchar_t c = abs[rl];
  return (c == 0 || c == L'\\') ? 0 : -1;
}

// DOS device names that must not be jail-checked (they are not paths).
static int W32IsDeviceName(const wchar_t *w) {
  return !wcscmp(w, L"NUL") || !wcscmp(w, L"CON") || !wcscmp(w, L"CONIN$") ||
         !wcscmp(w, L"CONOUT$") || !wcscmp(w, L"CONSOLE$");
}

static wchar_t *W32Path(const char *path, wchar_t buf[MAX_PATH]) {
  int n;
  wchar_t tmp[MAX_PATH];
  wchar_t joined[MAX_PATH];
  if (!path) return NULL;
  if (!strcmp(path, "/dev/null")) path = "NUL";
  if (path[0] == '/') {
    // absolute guest path: root at the jail root
    int root_comps;
    const wchar_t *root = W32JailRoot(&root_comps);
    if (getenv("WBOX_DEBUG_PATH"))
      fprintf(stderr, "[w32path] abs '%s'\n", path);
    n = MultiByteToWideChar(CP_UTF8, 0, path, -1, tmp, MAX_PATH);
    if (n <= 0) return NULL;
    if (wcslen(root) + wcslen(tmp) + 2 >= MAX_PATH) return NULL;
    wcscpy(joined, root);
    wcscat(joined, tmp);
    // C2: component-level normalization; reject any result that escapes
    // above the jail root (e.g. "/../outside" reaching the host).
    if (W32NormalizeW(joined, buf, root_comps)) {
      SetLastError(ERROR_ACCESS_DENIED);
      return NULL;
    }
  } else {
    n = MultiByteToWideChar(CP_UTF8, 0, path, -1, tmp, MAX_PATH);
    if (n <= 0) {
      // retry as ANSI
      n = MultiByteToWideChar(CP_ACP, 0, path, -1, tmp, MAX_PATH);
      if (n <= 0) return NULL;
    }
    // C2: cwd-relative paths may not climb above the cwd either
    if (W32NormalizeW(tmp, buf, 0)) {
      SetLastError(ERROR_ACCESS_DENIED);
      return NULL;
    }
    // C2 unified egress check: absolutize and prove the result stays
    // inside the jail root (defense in depth — the VFS layer should
    // never hand us ".." or absolute host paths outside the jail).
    if (!W32IsDeviceName(buf)) {
      if (buf[0] && buf[1] == L':') {
        wcscpy(joined, buf);  // already drive-prefixed absolute
      } else {
        DWORD m = GetCurrentDirectoryW(MAX_PATH, joined);
        if (!m || m >= MAX_PATH) return NULL;
        if (wcslen(joined) + wcslen(buf) + 2 >= MAX_PATH) return NULL;
        wcscat(joined, L"\\");
        wcscat(joined, buf);
      }
      {
        wchar_t full[MAX_PATH];
        if (W32NormalizeW(joined, full, 0) || W32WithinRoot(full)) {
          if (getenv("WBOX_DEBUG_PATH"))
            fprintf(stderr, "[w32path] DENY rel '%s' full '%S' root '%S'\n",
                    path, full, W32JailRoot(NULL));
          SetLastError(ERROR_ACCESS_DENIED);
          return NULL;
        }
      }
    }
  }
  // normalize slashes to backslashes (some win32 apis insist)
  for (n = 0; buf[n]; ++n)
    if (buf[n] == L'/') buf[n] = L'\\';
  return buf;
}

static HANDLE W32Handle(int fd);

// C2 (security-audit): real dirfd semantics for the *at family. A
// relative path with dirfd != AT_FDCWD is anchored at the directory the
// dirfd handle refers to (previously dirfd was silently ignored and the
// path resolved against the process-global cwd).
static wchar_t *W32ResolveAt(int dirfd, const char *path,
                             wchar_t buf[MAX_PATH]) {
  wchar_t dir[MAX_PATH], rel[MAX_PATH], joined[MAX_PATH];
  wchar_t *d;
  DWORD n;
  HANDLE h;
  if (!path) return NULL;
  if (path[0] == '/' || dirfd == AT_FDCWD) return W32Path(path, buf);
  h = W32Handle(dirfd);
  if (h == INVALID_HANDLE_VALUE || h == NULL) {
    SetLastError(ERROR_INVALID_HANDLE);
    return NULL;
  }
  n = GetFinalPathNameByHandleW(h, dir, MAX_PATH, FILE_NAME_NORMALIZED);
  if (!n || n >= MAX_PATH) return NULL;
  d = dir;
  if (!wcsncmp(d, L"\\\\?\\", 4)) d += 4;  // strip the \\?\ prefix
  if (MultiByteToWideChar(CP_UTF8, 0, path, -1, rel, MAX_PATH) <= 0 &&
      MultiByteToWideChar(CP_ACP, 0, path, -1, rel, MAX_PATH) <= 0) {
    return NULL;
  }
  if (wcslen(d) + wcslen(rel) + 2 >= MAX_PATH) return NULL;
  wcscpy(joined, d);
  wcscat(joined, L"\\");
  wcscat(joined, rel);
  // the anchor is absolute; ".." may not climb above the drive root.
  // (the VFS/hostfs layer already clamps ".." at the guest root — this
  // is defense in depth.)
  if (W32NormalizeW(joined, buf, 0)) {
    SetLastError(ERROR_ACCESS_DENIED);
    return NULL;
  }
  // C2 unified egress check: the resolved path must stay inside the
  // jail root, no matter which directory the dirfd points at (S3).
  if (W32WithinRoot(buf)) {
    if (getenv("WBOX_DEBUG_PATH"))
      fprintf(stderr, "[w32resolveat] DENY '%s' buf '%S' root '%S'\n", path,
              buf, W32JailRoot(NULL));
    SetLastError(ERROR_ACCESS_DENIED);
    return NULL;
  }
  return buf;
}

static DWORD W32Access(int flags, mode_t mode) {
  switch (flags & O_ACCMODE) {
    case O_RDONLY:
      return GENERIC_READ;
    case O_WRONLY:
      return GENERIC_WRITE;
    default:
      return GENERIC_READ | GENERIC_WRITE;
  }
}

static DWORD W32Disposition(int flags) {
  if ((flags & O_CREAT) && (flags & O_EXCL)) return CREATE_NEW;
  if ((flags & O_CREAT) && (flags & O_TRUNC)) return CREATE_ALWAYS;
  if (flags & O_CREAT) return OPEN_ALWAYS;
  if (flags & O_TRUNC) return TRUNCATE_EXISTING;
  return OPEN_EXISTING;
}

static int W32Err(void) {
  DWORD e = GetLastError();
  switch (e) {
    case ERROR_FILE_NOT_FOUND:
    case ERROR_PATH_NOT_FOUND:
      return ENOENT;
    case ERROR_ACCESS_DENIED:
      return EACCES;
    case ERROR_ALREADY_EXISTS:
    case ERROR_FILE_EXISTS:
      return EEXIST;
    case ERROR_INVALID_HANDLE:
      return EBADF;
    case ERROR_INVALID_PARAMETER:
      return EINVAL;
    case ERROR_NOT_ENOUGH_MEMORY:
      return ENOMEM;
    case ERROR_WRITE_PROTECT:
      return EROFS;
    case ERROR_SHARING_VIOLATION:
    case ERROR_LOCK_VIOLATION:
      return EACCES;
    case ERROR_HANDLE_EOF:
      return 0;
    case ERROR_BROKEN_PIPE:
      return EPIPE;
    case ERROR_DISK_FULL:
      return ENOSPC;
    case ERROR_DIR_NOT_EMPTY:
      return ENOTEMPTY;
        case ERROR_DIRECTORY:
      return ENOTDIR;
    case ERROR_IS_SUBSTED:
      return EEXIST;
    default:
      return EINVAL;
  }
}

static int W32ToCrt(HANDLE h, int flags) {
  int cflags = 0;
  if (flags & O_APPEND) cflags |= 0x0008;       // _O_APPEND
  if ((flags & O_ACCMODE) == O_RDONLY) cflags |= 0x0000;
  else if ((flags & O_ACCMODE) == O_WRONLY) cflags |= 0x0001;
  else cflags |= 0x0002;
  cflags |= 0x8000;  // _O_BINARY
  int fd = _open_osfhandle((intptr_t)h, cflags);
  if (fd == -1) CloseHandle(h);
  return fd;
}

static HANDLE W32Handle(int fd) {
  if (fd < 0) {
    SetLastError(ERROR_INVALID_HANDLE);
    return INVALID_HANDLE_VALUE;
  }
  HANDLE h = (HANDLE)_get_osfhandle(fd);
  if (h == INVALID_HANDLE_VALUE) SetLastError(ERROR_INVALID_HANDLE);
  return h;
}

int open(const char *path, int flags, ...) {
  mode_t mode = 0;
  if (flags & O_CREAT) {
    va_list ap;
    va_start(ap, flags);
    mode = (mode_t)va_arg(ap, int); /* mode_t promotes to int in varargs */
    va_end(ap);
  }
  return openat(AT_FDCWD, path, flags, mode);
}

int creat(const char *path, mode_t mode) {
  return open(path, O_WRONLY | O_CREAT | O_TRUNC, mode);
}

// Find the final "/dev/" path component, if any. Matches both the bare
// guest form ("/dev/null") and the prefixed host form produced by the
// VFS layer under BLINK_PREFIX ("/tmp/rootfs/dev/null" or, after win32
// translation, "Z:\tmp\rootfs\dev\null"). Returns a pointer to "dev/..."
// or NULL when the path is not a device path.
static const char *W32DevName(const char *path) {
  const char *p, *last = 0;
  if (!path) return 0;
  for (p = path; p[0] && p[1] && p[2] && p[3]; ++p) {
    if ((p == path || p[-1] == '/' || p[-1] == '\\' || p[-1] == ':') &&
        p[0] == 'd' && p[1] == 'e' && p[2] == 'v' &&
        (p[3] == '/' || p[3] == '\\')) {
      last = p;
      p += 2;
    }
  }
  return last;
}

// special device files faked for L1
static int W32OpenSpecial(const char *path, int flags) {
  HANDLE h;
  const char *dev = W32DevName(path);
  const char *name = dev ? dev + 4 : path;  // skip "dev/" if present
  if (dev && !strcmp(name, "null")) {
    h = CreateFileW(L"NUL", GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING,
                    0, NULL);
  } else if (!strcmp(path, "/dev/tty") || (dev && !strcmp(name, "tty"))) {
    h = CreateFileW(((flags & O_ACCMODE) == O_RDONLY) ? L"CONIN$" : L"CONOUT$",
                    GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE,
                    NULL, OPEN_EXISTING, 0, NULL);
  } else if (dev && (!strcmp(name, "urandom") || !strcmp(name, "random"))) {
    // fd 1000000 sentinel: handled in read()
    return 1000000;
  } else if (dev && !strcmp(name, "zero")) {
    return 1000001;
  } else if (dev && !strcmp(name, "full")) {
    return 1000002;
  } else {
    return -2;  // not special
  }
  if (h == INVALID_HANDLE_VALUE) {
    errno = W32Err();
    return -1;
  }
  return W32ToCrt(h, flags);
}

int openat(int dirfd, const char *path, int flags, ...) {
  wchar_t wbuf[MAX_PATH];
  HANDLE h;
  DWORD attr;
  int sp;
  mode_t mode = 0;
  if (flags & O_CREAT) {
    va_list ap;
    va_start(ap, flags);
    mode = (mode_t)va_arg(ap, int); /* mode_t promotes to int in varargs */
    va_end(ap);
  }
  (void)mode;
  if (!path) {
    errno = EFAULT;
    return -1;
  }
  if ((sp = W32OpenSpecial(path, flags)) != -2) return sp;
  if (flags & O_NOFOLLOW) {
    // no symlink support on win32 host (L1): plain open
  }
  if (!W32ResolveAt(dirfd, path, wbuf)) {
    errno = ENOENT;
    return -1;
  }
  attr = GetFileAttributesW(wbuf);
  if ((flags & O_DIRECTORY) && (attr == INVALID_FILE_ATTRIBUTES ||
                                !(attr & FILE_ATTRIBUTE_DIRECTORY))) {
    errno = ENOTDIR;
    return -1;
  }
  DWORD share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
  DWORD createflags = FILE_ATTRIBUTE_NORMAL;
  if (attr != INVALID_FILE_ATTRIBUTES && (attr & FILE_ATTRIBUTE_DIRECTORY)) {
    createflags |= FILE_FLAG_BACKUP_SEMANTICS;
  }
  h = CreateFileW(wbuf, W32Access(flags, mode), share, NULL,
                  W32Disposition(flags), createflags, NULL);
  if (h == INVALID_HANDLE_VALUE) {
    errno = W32Err();
    return -1;
  }
  if (flags & O_APPEND) SetFilePointer(h, 0, NULL, FILE_END);
  return W32ToCrt(h, flags);
}

// ---------------------------------------------------------------- io

// special fds 1000000..1000002 are fake device fds (no CRT fd)
static int IsSpecial(int fd) {
  return fd >= 1000000 && fd <= 1000002;
}

ssize_t read(int fd, void *buf, size_t n) {
  if (WboxSockIsFd(fd)) return WboxSockRead(fd, buf, n);  // feat/net
  if (IsSpecial(fd)) {
    if (fd == 1000001) {  // zero
      memset(buf, 0, n);
      return n;
    }
    if (fd == 1000000) {  // urandom
      NTSTATUS st = BCryptGenRandom(NULL, buf, n, BCRYPT_USE_SYSTEM_PREFERRED_RNG);
      if (st != 0) {
        errno = EIO;
        return -1;
      }
      return n;
    }
    return 0;  // full: reads give eof
  }
  HANDLE h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return -1;
  }
  DWORD got = 0;
  if (!ReadFile(h, buf, n > 0x7fffffff ? 0x7fffffff : (DWORD)n, &got, NULL)) {
    DWORD e = GetLastError();
    if (e == ERROR_BROKEN_PIPE) return 0;
    if (e == ERROR_HANDLE_EOF) return 0;
    errno = W32Err();
    return -1;
  }
  return got;
}

ssize_t write(int fd, const void *buf, size_t n) {
  if (WboxSockIsFd(fd)) return WboxSockWrite(fd, buf, n);  // feat/net
  if (IsSpecial(fd)) {
    if (fd == 1000002) {  // full
      errno = ENOSPC;
      return -1;
    }
    return n;
  }
  HANDLE h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return -1;
  }
  DWORD put = 0;
  if (!WriteFile(h, buf, n > 0x7fffffff ? 0x7fffffff : (DWORD)n, &put, NULL)) {
    errno = W32Err();
    if (errno == 0) errno = EIO;
    return -1;
  }
  return put;
}

int close(int fd) {
  if (WboxSockClose(fd)) return 0;    // feat/net: socket fds
  if (WboxEpollClose(fd)) return 0;   // feat/net: epoll fds
  if (IsSpecial(fd)) return 0;
  return _close(fd) ? (errno = EBADF, -1) : 0;
}

off_t lseek(int fd, off_t off, int whence) {
  if (IsSpecial(fd)) return 0;
  HANDLE h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return -1;
  }
  LARGE_INTEGER d, r;
  d.QuadPart = off;
  DWORD m = whence == SEEK_SET ? FILE_BEGIN : whence == SEEK_CUR ? FILE_CURRENT
                                                                 : FILE_END;
  if (!SetFilePointerEx(h, d, &r, m)) {
    errno = W32Err();
    return -1;
  }
  return r.QuadPart;
}

ssize_t pread(int fd, void *buf, size_t n, off_t off) {
  if (IsSpecial(fd)) return read(fd, buf, n);
  HANDLE h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return -1;
  }
  OVERLAPPED ov;
  memset(&ov, 0, sizeof(ov));
  /* off_t is 32-bit on windows-gnu; widen before shifting (>>32 on a
     32-bit value is UB). Files >=4GiB still need the CRT off_t fixed. */
  ov.Offset = (DWORD)(uint64_t)off;
  ov.OffsetHigh = (DWORD)((uint64_t)off >> 32);
  DWORD got = 0;
  if (!ReadFile(h, buf, n > 0x7fffffff ? 0x7fffffff : (DWORD)n, &got, &ov)) {
    DWORD e = GetLastError();
    if (e == ERROR_HANDLE_EOF) return got ? got : 0;
    errno = W32Err();
    return -1;
  }
  return got;
}

ssize_t pwrite(int fd, const void *buf, size_t n, off_t off) {
  HANDLE h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return -1;
  }
  OVERLAPPED ov;
  memset(&ov, 0, sizeof(ov));
  /* see pread(): widen the 32-bit off_t before shifting. */
  ov.Offset = (DWORD)(uint64_t)off;
  ov.OffsetHigh = (DWORD)((uint64_t)off >> 32);
  DWORD put = 0;
  if (!WriteFile(h, buf, n > 0x7fffffff ? 0x7fffffff : (DWORD)n, &put, &ov)) {
    errno = W32Err();
    return -1;
  }
  return put;
}

ssize_t readv(int fd, const struct iovec *iov, int n) {
  ssize_t total = 0;
  int i;
  for (i = 0; i < n; ++i) {
    ssize_t rc = read(fd, iov[i].iov_base, iov[i].iov_len);
    if (rc < 0) return total ? total : -1;
    total += rc;
    if ((size_t)rc < iov[i].iov_len) break;
  }
  return total;
}

ssize_t writev(int fd, const struct iovec *iov, int n) {
  ssize_t total = 0;
  int i;
  for (i = 0; i < n; ++i) {
    ssize_t rc = write(fd, iov[i].iov_base, iov[i].iov_len);
    if (rc < 0) return total ? total : -1;
    total += rc;
    if ((size_t)rc < iov[i].iov_len) break;
  }
  return total;
}

ssize_t preadv(int fd, const struct iovec *iov, int n, off_t off) {
  ssize_t total = 0;
  int i;
  for (i = 0; i < n; ++i) {
    ssize_t rc = pread(fd, iov[i].iov_base, iov[i].iov_len, off + total);
    if (rc < 0) return total ? total : -1;
    total += rc;
    if ((size_t)rc < iov[i].iov_len) break;
  }
  return total;
}

ssize_t pwritev(int fd, const struct iovec *iov, int n, off_t off) {
  ssize_t total = 0;
  int i;
  for (i = 0; i < n; ++i) {
    ssize_t rc = pwrite(fd, iov[i].iov_base, iov[i].iov_len, off + total);
    if (rc < 0) return total ? total : -1;
    total += rc;
    if ((size_t)rc < iov[i].iov_len) break;
  }
  return total;
}

int dup(int fd) {
  if (IsSpecial(fd)) return fd + 10;  // fake: another special id band
  HANDLE h = W32Handle(fd), duph;
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return -1;
  }
  if (!DuplicateHandle(GetCurrentProcess(), h, GetCurrentProcess(), &duph, 0,
                       FALSE, DUPLICATE_SAME_ACCESS)) {
    errno = W32Err();
    return -1;
  }
  return W32ToCrt(duph, 0);
}

int dup2(int fd, int fd2) {
  if (fd == fd2) return fd2;
  if (IsSpecial(fd)) {
    // redirect: replace target handle with our fake marker is impossible;
    // busybox redirects to /dev/null etc via open+dup2 so map fake->NUL
    HANDLE h;
    switch (fd % 10) {
      case 0:  // urandom dup: just give NUL
      case 2:  // full
      case 1:  // zero
      default:
        h = CreateFileW(L"NUL", GENERIC_READ | GENERIC_WRITE,
                        FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING,
                        0, NULL);
        break;
    }
    if (h == INVALID_HANDLE_VALUE) {
      errno = W32Err();
      return -1;
    }
    // replace fd2's handle: close and dup into it via _dup2 of a temp crt fd
    int tmp = W32ToCrt(h, 0);
    if (tmp == -1) return -1;
    if (_dup2(tmp, fd2)) {
      _close(tmp);
      errno = EBADF;
      return -1;
    }
    _close(tmp);
    return fd2;
  }
  if (_dup2(fd, fd2)) {
    errno = EBADF;
    return -1;
  }
  return fd2;
}

int dup3(int fd, int fd2, int flags) {
  if (fd == fd2) {
    errno = EINVAL;
    return -1;
  }
  return dup2(fd, fd2);
}

int pipe(int fds[2]) {
  return pipe2(fds, 0);
}

int pipe2(int fds[2], int flags) {
  HANDLE r, w;
  SECURITY_ATTRIBUTES sa;
  sa.nLength = sizeof(sa);
  sa.lpSecurityDescriptor = NULL;
  sa.bInheritHandle = FALSE;
  if (!CreatePipe(&r, &w, &sa, 0)) {
    errno = W32Err();
    return -1;
  }
  fds[0] = W32ToCrt(r, O_RDONLY);
  fds[1] = W32ToCrt(w, O_WRONLY);
  if (fds[0] == -1 || fds[1] == -1) {
    if (fds[0] != -1) _close(fds[0]);
    if (fds[1] != -1) _close(fds[1]);
    errno = EMFILE;
    return -1;
  }
  return 0;
}

int fcntl(int fd, int cmd, ...) {
  int arg = 0;
  va_list ap;
  va_start(ap, cmd);
  if (cmd == F_SETFL || cmd == F_SETFD || cmd == F_SETLK || cmd == F_SETLKW ||
      cmd == F_DUPFD || cmd == F_DUPFD_CLOEXEC)
    arg = va_arg(ap, int);
  va_end(ap);
  // feat/net: socket fds get real O_NONBLOCK (FIONBIO) semantics
  if (cmd == F_GETFL || cmd == F_SETFL || cmd == F_GETFD || cmd == F_SETFD) {
    int src = WboxSockFcntl(fd, cmd, arg);
    if (src != -2) return src;
  }
  switch (cmd) {
    case F_GETFL:
      return O_RDWR;  // good enough for L1 (busybox checks & clears NONBLOCK)
    case F_SETFL:
      return 0;  // ignore NONBLOCK toggling (console/pipes stay blocking)
    case F_GETFD:
      return 0;  // FD_CLOEXEC meaningless without fork
    case F_SETFD:
      return 0;
    case F_DUPFD:
    case F_DUPFD_CLOEXEC: {
      int nfd = dup(fd);
      // NOTE: does not honor min-fd argument; L1 acceptable
      return nfd;
    }
    case F_GETLK:
    case F_SETLK:
    case F_SETLKW:
      return 0;
    default:
      errno = EINVAL;
      return -1;
  }
}

int isatty(int fd) {
  HANDLE h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE) return 0;
  DWORD m;
  return GetConsoleMode(h, &m) ? 1 : 0;
}

int fsync(int fd) {
  HANDLE h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return -1;
  }
  return FlushFileBuffers(h) ? 0 : (errno = W32Err(), -1);
}

int fdatasync(int fd) {
  return fsync(fd);
}

int ftruncate(int fd, off_t len) {
  HANDLE h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return -1;
  }
  LARGE_INTEGER cur, pos, r;
  if (!SetFilePointerEx(h, (LARGE_INTEGER){0}, &cur, FILE_CURRENT)) {
    errno = W32Err();
    return -1;
  }
  pos.QuadPart = len;
  if (!SetFilePointerEx(h, pos, &r, FILE_BEGIN) || !SetEndOfFile(h)) {
    errno = W32Err();
    return -1;
  }
  SetFilePointerEx(h, cur, &r, FILE_BEGIN);
  return 0;
}

int truncate(const char *path, off_t len) {
  int fd = open(path, O_WRONLY);
  if (fd == -1) return -1;
  int rc = ftruncate(fd, len);
  int e = errno;
  close(fd);
  errno = e;
  return rc;
}

int flock(int fd, int op) {
  return 0;  // L1: no advisory locking
}

int lockf(int fd, int cmd, off_t len) {
  return 0;
}

// ---------------------------------------------------------------- stat

static void W32FillStat(struct stat *st, HANDLE h, const wchar_t *wpath) {
  FILETIME ct, at, wt;
  BY_HANDLE_FILE_INFORMATION info;
  int have_info = 0;
  memset(st, 0, sizeof(*st));
  if (h && GetFileInformationByHandle(h, &info)) {
    have_info = 1;
    st->st_nlink = info.nNumberOfLinks;
    st->st_ino = ((uint64_t)info.nFileIndexHigh << 32) | info.nFileIndexLow;
    st->st_size =
        (off_t)(((uint64_t)info.nFileSizeHigh << 32) | info.nFileSizeLow);
    st->st_dev = info.dwVolumeSerialNumber & 0xff;
    ct = info.ftCreationTime;
    at = info.ftLastAccessTime;
    wt = info.ftLastWriteTime;
  } else if (h) {
    // wine fallback: GetFileInformationByHandle may fail on some handles
    LARGE_INTEGER sz;
    if (GetFileSizeEx(h, &sz)) st->st_size = (off_t)sz.QuadPart;
    if (!GetFileTime(h, &ct, &at, &wt)) {
      memset(&ct, 0, sizeof(ct));
      at = wt = ct;
    }
  } else {
    memset(&ct, 0, sizeof(ct));
    at = wt = ct;
  }
  DWORD attr = wpath ? GetFileAttributesW(wpath)
           : have_info ? info.dwFileAttributes
                       : 0;
  DWORD ft = h ? GetFileType(h) : FILE_TYPE_DISK;
  if (ft == FILE_TYPE_CHAR) {
    st->st_mode = S_IFCHR | 0666;
  } else if (ft == FILE_TYPE_PIPE) {
    st->st_mode = S_IFIFO | 0666;
  } else if ((attr != INVALID_FILE_ATTRIBUTES) &&
             (attr & FILE_ATTRIBUTE_DIRECTORY)) {
    st->st_mode = S_IFDIR | 0755;
    st->st_nlink = 2;
  } else {
    st->st_mode = S_IFREG;
    st->st_mode |= (attr != INVALID_FILE_ATTRIBUTES &&
                    (attr & FILE_ATTRIBUTE_READONLY))
                       ? 0444
                       : 0666;
    // executable bit by extension
    if (wpath) {
      size_t n = wcslen(wpath);
      if ((n > 4 && (!wcscmp(wpath + n - 4, L".exe") ||
                     !wcscmp(wpath + n - 4, L".com"))) ||
          !(attr & FILE_ATTRIBUTE_READONLY)) {
        st->st_mode |= 0111;
      }
    }
    if (!st->st_nlink) st->st_nlink = 1;
  }
  // win32 filetime (100ns since 1601) -> unix timespec
  uint64_t t;
  t = ((uint64_t)ct.dwHighDateTime << 32) | ct.dwLowDateTime;
  st->st_ctim.tv_sec = t / 10000000 - 11644473600LL;
  st->st_ctim.tv_nsec = (t % 10000000) * 100;
  t = ((uint64_t)at.dwHighDateTime << 32) | at.dwLowDateTime;
  st->st_atim.tv_sec = t / 10000000 - 11644473600LL;
  st->st_atim.tv_nsec = (t % 10000000) * 100;
  t = ((uint64_t)wt.dwHighDateTime << 32) | wt.dwLowDateTime;
  st->st_mtim.tv_sec = t / 10000000 - 11644473600LL;
  st->st_mtim.tv_nsec = (t % 10000000) * 100;
  st->st_blksize = 4096;
  st->st_blocks = (st->st_size + 511) / 512;
}

int fstat(int fd, struct stat *st) {
  unsigned sockmode;
  if (WboxSockFstatMode(fd, &sockmode)) {  // feat/net
    memset(st, 0, sizeof(*st));
    st->st_mode = sockmode;
    st->st_blksize = 4096;
    st->st_nlink = 1;
    return 0;
  }
  if (WboxEpollIsFd(fd)) {  // feat/net
    memset(st, 0, sizeof(*st));
    st->st_mode = S_IFCHR | 0600;
    st->st_blksize = 4096;
    st->st_nlink = 1;
    return 0;
  }
  if (IsSpecial(fd)) {
    memset(st, 0, sizeof(*st));
    st->st_mode = S_IFCHR | 0666;
    st->st_blksize = 4096;
    return 0;
  }
  HANDLE h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return -1;
  }
  W32FillStat(st, h, NULL);
  return 0;
}

int stat(const char *path, struct stat *st) {
  return fstatat(AT_FDCWD, path, st, 0);
}

int lstat(const char *path, struct stat *st) {
  return fstatat(AT_FDCWD, path, st, AT_SYMLINK_NOFOLLOW);
}

int fstatat(int dirfd, const char *path, struct stat *st, int flags) {
  wchar_t wbuf[MAX_PATH];
  if (!W32ResolveAt(dirfd, path, wbuf)) {
    errno = ENOENT;
    return -1;
  }
  DWORD attr = GetFileAttributesW(wbuf);
  if (attr == INVALID_FILE_ATTRIBUTES) {
    errno = W32Err();
    return -1;
  }
  HANDLE h = CreateFileW(wbuf, GENERIC_READ,
                         FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                         NULL, OPEN_EXISTING,
                         (attr & FILE_ATTRIBUTE_DIRECTORY)
                             ? FILE_FLAG_BACKUP_SEMANTICS
                             : FILE_ATTRIBUTE_NORMAL,
                         NULL);
  W32FillStat(st, h != INVALID_HANDLE_VALUE ? h : NULL, wbuf);
  if (h != INVALID_HANDLE_VALUE) CloseHandle(h);
  return 0;
}

// ---------------------------------------------------------------- fs ops

int access(const char *path, int mode) {
  return faccessat(AT_FDCWD, path, mode, 0);
}

int faccessat(int dirfd, const char *path, int mode, int flags) {
  wchar_t wbuf[MAX_PATH];
  if (!W32ResolveAt(dirfd, path, wbuf)) {
    errno = ENOENT;
    return -1;
  }
  DWORD attr = GetFileAttributesW(wbuf);
  if (attr == INVALID_FILE_ATTRIBUTES) {
    errno = W32Err();
    return -1;
  }
  if ((mode & W_OK) && (attr & FILE_ATTRIBUTE_READONLY)) {
    errno = EACCES;
    return -1;
  }
  return 0;
}

int unlink(const char *path) {
  wchar_t wbuf[MAX_PATH];
  if (!W32Path(path, wbuf) || !DeleteFileW(wbuf)) {
    errno = W32Err();
    return -1;
  }
  return 0;
}

int unlinkat(int dirfd, const char *path, int flags) {
  wchar_t wbuf[MAX_PATH];
  if (!W32ResolveAt(dirfd, path, wbuf)) {
    errno = ENOENT;
    return -1;
  }
  if (flags & AT_REMOVEDIR) {
    if (!RemoveDirectoryW(wbuf)) {
      errno = W32Err();
      return -1;
    }
    return 0;
  }
  if (!DeleteFileW(wbuf)) {
    errno = W32Err();
    return -1;
  }
  return 0;
}

int rmdir(const char *path) {
  wchar_t wbuf[MAX_PATH];
  if (!W32Path(path, wbuf) || !RemoveDirectoryW(wbuf)) {
    errno = W32Err();
    return -1;
  }
  return 0;
}

int rename(const char *old, const char *new_) {
  wchar_t wo[MAX_PATH], wn[MAX_PATH];
  if (!W32Path(old, wo) || !W32Path(new_, wn)) {
    errno = ENOENT;
    return -1;
  }
  if (!MoveFileExW(wo, wn, MOVEFILE_REPLACE_EXISTING | MOVEFILE_COPY_ALLOWED)) {
    errno = W32Err();
    return -1;
  }
  return 0;
}

int renameat(int oldfd, const char *old, int newfd, const char *new_) {
  wchar_t wo[MAX_PATH], wn[MAX_PATH];
  if (!W32ResolveAt(oldfd, old, wo) || !W32ResolveAt(newfd, new_, wn)) {
    errno = ENOENT;
    return -1;
  }
  if (!MoveFileExW(wo, wn, MOVEFILE_REPLACE_EXISTING | MOVEFILE_COPY_ALLOWED)) {
    errno = W32Err();
    return -1;
  }
  return 0;
}

int renameat2(int oldfd, const char *old, int newfd, const char *new_,
              unsigned flags) {
  return renameat(oldfd, old, newfd, new_);  // RENAME_NOREPLACE etc. unimpl
}

int mkdir(const char *path, mode_t mode) {
  wchar_t wbuf[MAX_PATH];
  if (!W32Path(path, wbuf)) {
    errno = ENOENT;
    return -1;
  }
  if (!CreateDirectoryW(wbuf, NULL)) {
    errno = W32Err();
    return -1;
  }
  return 0;
}

int mkdirat(int dirfd, const char *path, mode_t mode) {
  wchar_t wbuf[MAX_PATH];
  if (!W32ResolveAt(dirfd, path, wbuf)) {
    errno = ENOENT;
    return -1;
  }
  if (!CreateDirectoryW(wbuf, NULL)) {
    errno = W32Err();
    return -1;
  }
  return 0;
}

int chmod(const char *path, mode_t mode) {
  wchar_t wbuf[MAX_PATH];
  if (!W32Path(path, wbuf)) {
    errno = ENOENT;
    return -1;
  }
  DWORD attr = GetFileAttributesW(wbuf);
  if (attr == INVALID_FILE_ATTRIBUTES) {
    errno = W32Err();
    return -1;
  }
  if (mode & 0200) attr &= ~FILE_ATTRIBUTE_READONLY;
  else attr |= FILE_ATTRIBUTE_READONLY;
  if (!SetFileAttributesW(wbuf, attr)) {
    errno = W32Err();
    return -1;
  }
  return 0;
}

int fchmod(int fd, mode_t mode) {
  return 0;  // L1
}

int fchmodat(int dirfd, const char *path, mode_t mode, int flags) {
  wchar_t wbuf[MAX_PATH];
  if (!W32ResolveAt(dirfd, path, wbuf)) {
    errno = ENOENT;
    return -1;
  }
  DWORD attr = GetFileAttributesW(wbuf);
  if (attr == INVALID_FILE_ATTRIBUTES) {
    errno = W32Err();
    return -1;
  }
  if (mode & 0200) attr &= ~FILE_ATTRIBUTE_READONLY;
  else attr |= FILE_ATTRIBUTE_READONLY;
  if (!SetFileAttributesW(wbuf, attr)) {
    errno = W32Err();
    return -1;
  }
  return 0;
}

mode_t umask(mode_t m) {
  static mode_t cur = 022;
  mode_t old = cur;
  cur = m & 0777;
  return old;
}

int link(const char *old, const char *new_) {
  wchar_t wo[MAX_PATH], wn[MAX_PATH];
  if (!W32Path(old, wo) || !W32Path(new_, wn)) {
    errno = ENOENT;
    return -1;
  }
  if (!CreateHardLinkW(wn, wo, NULL)) {
    errno = W32Err();
    return -1;
  }
  return 0;
}

int linkat(int ofd, const char *old, int nfd, const char *new_, int flags) {
  wchar_t wo[MAX_PATH], wn[MAX_PATH];
  if (!W32ResolveAt(ofd, old, wo) || !W32ResolveAt(nfd, new_, wn)) {
    errno = ENOENT;
    return -1;
  }
  if (!CreateHardLinkW(wn, wo, NULL)) {
    errno = W32Err();
    return -1;
  }
  return 0;
}

int symlink(const char *target, const char *path) {
  errno = EPERM;  // needs privileges; guests should cope
  return -1;
}

int symlinkat(const char *target, int dirfd, const char *path) {
  return symlink(target, path);
}

ssize_t readlink(const char *path, char *buf, size_t size) {
  errno = EINVAL;  // no symlinks on win32 host
  return -1;
}

ssize_t readlinkat(int dirfd, const char *path, char *buf, size_t size) {
  return readlink(path, buf, size);
}

// NB (C2, security-audit): guest chdir NEVER reaches here — blink's
// VfsChdir tracks cwd per guest context (g_cwdinfo) at the VFS layer.
// This host chdir is only used by overlay setup during VfsInit, i.e. by
// trusted mount-source paths, so the process-global cwd is not a
// guest-controlled resolution base. All guest-relative host paths go
// through the *at functions, which honor dirfd (W32ResolveAt).
int chdir(const char *path) {
  wchar_t wbuf[MAX_PATH];
  if (!W32Path(path, wbuf) || !SetCurrentDirectoryW(wbuf)) {
    errno = W32Err();
    return -1;
  }
  return 0;
}

int fchdir(int fd) {
  errno = ENOSYS;
  return -1;
}

char *getcwd(char *buf, size_t size) {
  wchar_t wbuf[MAX_PATH];
  DWORD n = GetCurrentDirectoryW(MAX_PATH, wbuf);
  if (!n) {
    errno = W32Err();
    return NULL;
  }
  // convert backslashes and prepend drive-relative root:
  // wine cwd like Z:\tmp\wbox5 -> /tmp/wbox5 (drop drive letter)
  wchar_t *p = wbuf;
  if (n >= 2 && wbuf[1] == L':') p = wbuf + 2;
  for (wchar_t *q = p; *q; ++q)
    if (*q == L'\\') *q = L'/';
  int m = WideCharToMultiByte(CP_UTF8, 0, p, -1, buf, size, NULL, NULL);
  if (m <= 0) {
    errno = ERANGE;
    return NULL;
  }
  return buf;
}

int chroot(const char *path) {
  errno = ENOSYS;
  return -1;
}

int mkfifo(const char *path, mode_t mode) {
  errno = ENOSYS;
  return -1;
}

int mkfifoat(int d, const char *p, mode_t m) {
  return mkfifo(p, m);
}

int mknod(const char *p, mode_t m, dev_t d) {
  errno = ENOSYS;
  return -1;
}

int mknodat(int d, const char *p, mode_t m, dev_t dv) {
  return mknod(p, m, dv);
}

int mkstemp(char *tmpl) {
  // simplistic: randomize XXXXXX
  size_t n = strlen(tmpl);
  if (n < 6) {
    errno = EINVAL;
    return -1;
  }
  for (int tries = 0; tries < 100; ++tries) {
    unsigned r;
    BCryptGenRandom(NULL, (PUCHAR)&r, sizeof(r), BCRYPT_USE_SYSTEM_PREFERRED_RNG);
    static const char dig[] = "abcdefghijklmnopqrstuvwxyz0123456789";
    for (int i = 0; i < 6; ++i) tmpl[n - 6 + i] = dig[(r >> (i * 5)) & 31];
    int fd = open(tmpl, O_RDWR | O_CREAT | O_EXCL, 0600);
    if (fd != -1) return fd;
    if (errno != EEXIST) return -1;
  }
  errno = EEXIST;
  return -1;
}

int futimens(int fd, const struct timespec ts[2]) {
  HANDLE h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return -1;
  }
  FILETIME at, wt;
  uint64_t t;
  t = (uint64_t)(ts ? ts[0].tv_sec : 0);
  t = (t + 11644473600ULL) * 10000000ULL + (ts ? ts[0].tv_nsec / 100 : 0);
  at.dwLowDateTime = t;
  at.dwHighDateTime = t >> 32;
  t = (uint64_t)(ts ? ts[1].tv_sec : 0);
  t = (t + 11644473600ULL) * 10000000ULL + (ts ? ts[1].tv_nsec / 100 : 0);
  wt.dwLowDateTime = t;
  wt.dwHighDateTime = t >> 32;
  return SetFileTime(h, NULL, &at, &wt) ? 0 : (errno = W32Err(), -1);
}

int utimensat(int dirfd, const char *path, const struct timespec ts[2],
              int flags) {
  int fd = openat(dirfd, path, O_WRONLY);
  if (fd == -1) return -1;
  int rc = futimens(fd, ts);
  int e = errno;
  close(fd);
  errno = e;
  return rc;
}

// ---------------------------------------------------------------- poll/select

int poll(struct pollfd *pfds, nfds_t n, int timeout) {
  // files & console always ready; pipes peeked; sockets via WSAPoll (feat/net)
  DWORD deadline = timeout >= 0 ? GetTickCount() + timeout : 0;
  int nsock = 0;
  for (nfds_t i = 0; i < n; ++i)
    if (pfds[i].fd >= 0 && WboxSockIsFd(pfds[i].fd)) ++nsock;
  for (;;) {
    int nready = 0;
    for (nfds_t i = 0; i < n; ++i) {
      struct pollfd *p = pfds + i;
      p->revents = 0;
      if (p->fd < 0 || WboxSockIsFd(p->fd)) continue;
      if (IsSpecial(p->fd)) {
        p->revents = p->events & (POLLIN | POLLOUT);
      } else {
        HANDLE h = W32Handle(p->fd);
        if (h == INVALID_HANDLE_VALUE) {
          p->revents = POLLNVAL;
        } else {
          DWORD ft = GetFileType(h);
          if (ft == FILE_TYPE_PIPE) {
            DWORD avail = 0;
            if (PeekNamedPipe(h, NULL, 0, NULL, &avail, NULL)) {
              if (avail) p->revents |= p->events & POLLIN;
              p->revents |= p->events & POLLOUT;
            } else {
              // feat/listener: wine cannot PeekNamedPipe stdio handles that
              // wrap a unix fifo/socketpair (ERROR_NOT_SUPPORTED). Reporting
              // POLLERR made guests (busybox nc) read(0) and block forever
              // on an empty fifo while socket data went unserviced. Fall
              // back to the handle wait state: wine pipes/fifos are signaled
              // when data is available or the writer is gone.
              DWORD werr = GetLastError();
              if (werr == ERROR_PIPE_NOT_CONNECTED ||
                  werr == ERROR_BAD_PIPE || werr == ERROR_BROKEN_PIPE ||
                  werr == ERROR_NO_DATA) {
                p->revents = POLLHUP;
              } else {
                DWORD wrc = WaitForSingleObject(h, 0);
                if (wrc == WAIT_OBJECT_0) {
                  // readable or hung up; read() disambiguates (0 == EOF)
                  p->revents |= p->events & (POLLIN | POLLOUT);
                } else if (wrc == WAIT_TIMEOUT) {
                  p->revents = 0;
                } else {
                  p->revents = POLLERR;
                }
              }
            }
          } else {
            p->revents = p->events & (POLLIN | POLLOUT);
          }
        }
      }
      if (p->revents) ++nready;
    }
    // socket fds: level-triggered WSAPoll. slice the wait so the non-socket
    // fds above are re-checked regularly (shared wait primitive).
    if (nsock) {
      int slice = 0;
      if (!nready && timeout != 0) {
        slice = 10;
        if (timeout > 0) {
          DWORD now = GetTickCount();
          long left = (long)(deadline - now);
          if (left < 0) left = 0;
          if (left < slice) slice = (int)left;
        }
      }
      int src = WboxSockPoll(pfds, n, slice);
      if (src < 0) return -1;
      nready += src;
    }
    if (nready || timeout == 0) return nready;
    if (timeout > 0 && GetTickCount() >= deadline) return 0;
    if (!nsock) Sleep(timeout > 10 ? 10 : 1);
  }
}

int ppoll(struct pollfd *pfds, nfds_t n, const struct timespec *ts,
          const sigset_t *mask) {
  int timeout = ts ? ts->tv_sec * 1000 + ts->tv_nsec / 1000000 : -1;
  return poll(pfds, n, timeout);
}

int select(int n, fd_set *r, fd_set *w, fd_set *x, struct timeval *tv) {
  int count = 0;
  int timeout = tv ? tv->tv_sec * 1000 + tv->tv_usec / 1000 : -1;
  DWORD deadline = timeout >= 0 ? GetTickCount() + timeout : 0;
  fd_set rr, ww;
  for (;;) {
    count = 0;
    if (r) {
      FD_ZERO(&rr);
      for (int fd = 0; fd < n; ++fd)
        if (FD_ISSET(fd, r)) {
          struct pollfd p = {fd, POLLIN, 0};
          poll(&p, 1, 0);
          if (p.revents & (POLLIN | POLLERR | POLLHUP)) {
            FD_SET(fd, &rr);
            ++count;
          }
        }
    }
    if (w) {
      FD_ZERO(&ww);
      for (int fd = 0; fd < n; ++fd)
        if (FD_ISSET(fd, w)) {
          struct pollfd p = {fd, POLLOUT, 0};
          poll(&p, 1, 0);
          if (p.revents & POLLOUT) {
            FD_SET(fd, &ww);
            ++count;
          }
        }
    }
    if (count || timeout == 0) break;
    if (timeout > 0 && GetTickCount() >= deadline) break;
    Sleep(5);
  }
  if (r) *r = rr;
  if (w) *w = ww;
  if (x) FD_ZERO(x);
  return count;
}

int pselect(int n, fd_set *r, fd_set *w, fd_set *x, const struct timespec *ts,
            const sigset_t *mask) {
  struct timeval tv;
  tv.tv_sec = ts ? ts->tv_sec : 0;
  tv.tv_usec = ts ? ts->tv_nsec / 1000 : 0;
  return select(n, r, w, x, ts ? &tv : NULL);
}

int ioctl(int fd, unsigned long req, ...) {
  void *arg;
  va_list ap;
  va_start(ap, req);
  arg = va_arg(ap, void *);
  va_end(ap);
  switch (req) {
    case TIOCGWINSZ: {
      struct winsize *ws = arg;
      CONSOLE_SCREEN_BUFFER_INFO ci;
      HANDLE h = W32Handle(fd);
      if (h != INVALID_HANDLE_VALUE &&
          GetConsoleScreenBufferInfo(h, &ci)) {
        ws->ws_col = ci.srWindow.Right - ci.srWindow.Left + 1;
        ws->ws_row = ci.srWindow.Bottom - ci.srWindow.Top + 1;
      } else {
        ws->ws_col = 80;
        ws->ws_row = 25;
      }
      ws->ws_xpixel = ws->ws_ypixel = 0;
      return 0;
    }
    case TIOCSWINSZ:
      return 0;
    case FIONREAD: {
      DWORD avail = 0;
      int src = WboxSockFionread(fd, (int *)arg);  // feat/net: sockets
      if (src != -2) return src;
      HANDLE h = W32Handle(fd);
      if (h == INVALID_HANDLE_VALUE) {
        errno = EBADF;
        return -1;
      }
      if (GetFileType(h) == FILE_TYPE_PIPE) {
        PeekNamedPipe(h, NULL, 0, NULL, &avail, NULL);
      } else {
        LARGE_INTEGER size, pos;
        SetFilePointerEx(h, (LARGE_INTEGER){0}, &pos, FILE_CURRENT);
        GetFileSizeEx(h, &size);
        avail = (DWORD)(size.QuadPart > pos.QuadPart ? size.QuadPart - pos.QuadPart : 0);
      }
      *(int *)arg = avail;
      return 0;
    }
    case TIOCGPGRP:
      *(pid_t *)arg = getpgrp();
      return 0;
    case TIOCSPGRP:
      return 0;
    default:
      errno = ENOTTY;
      return -1;
  }
}
