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
#include <wctype.h>
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
  wchar_t tmp[W32_PATH_MAX];
  wchar_t *comps[W32_PATH_MAX / 2];
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
  if (wcslen(s) >= W32_PATH_MAX) return -1;
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

static const wchar_t *W32StripExtendedPrefix(const wchar_t *w) {
  return !wcsncmp(w, L"\\\\?\\", 4) ? w + 4 : w;
}

static int W32EscapeName(wchar_t *w);

// Shared escape+join+normalize step of THE path-resolution entry points
// (W32Path for cwd/absolute, W32ResolveAt for the dirfd-anchored *at
// family): escapes `rel` in place (%XXXX form), joins it under `anchor`
// with one separator, and component-normalizes the result, rejecting any
// ".." that pops above `floor` anchor components. Sets LastError on
// failure. Previously this sequence was open-coded twice.
static int W32JoinNorm(const wchar_t *anchor, wchar_t *rel, int floor,
                       wchar_t out[W32_PATH_MAX]);

// C2 (security-audit): THE jail root. Every host path that reaches a
// Win32 API must textually resolve inside this directory — this is the
// single egress check shared by all path entries (W32Path for cwd-based
// and absolute paths, W32ResolveAt for the dirfd-anchored *at family;
// both share the W32JoinNorm escape/join/normalize step).
// VfsInit publishes the VFS root via WBOX_ROOT (BLINK_PREFIX, or the
// launch cwd under jail-by-default), so the VFS layer and this fd layer
// never disagree about what "outside the sandbox" means.
// (Audit: w32stubs.c's opendir/realpath intentionally bypass this — they
// serve the VFS hostfs with already host-rooted paths, a different layer.)
static const wchar_t *W32JailRoot(int *comps) {
  static wchar_t root[W32_PATH_MAX];
  static int root_init, root_comps;
  if (!root_init) {
    char *r = getenv("WBOX_ROOT");
    if (!r || !*r) {
      // jail-by-default fallback (VfsInit not run yet): the launch cwd
      DWORD m = GetCurrentDirectoryW(W32_PATH_MAX, root);
      if (!m || m >= W32_PATH_MAX) {
        wcscpy(root, L"Z:\\");  // last resort: wine unix fs root
      }
    } else {
      root[0] = 0;
      MultiByteToWideChar(CP_UTF8, 0, r, -1, root, W32_PATH_MAX - 1);
      root[W32_PATH_MAX - 1] = 0;
    }
    if (!wcsncmp(root, L"\\\\?\\", 4)) {
      memmove(root, root + 4, (wcslen(root + 4) + 1) * sizeof(*root));
    }
    for (int n = 0; root[n]; ++n)
      if (root[n] == L'/') root[n] = L'\\';
    // strip trailing backslashes ("Z:\" keeps its root slash)
    size_t l = wcslen(root);
    while (l > 3 && root[l - 1] == L'\\') root[--l] = 0;
    // F7/F8: resolved host paths are compared in ESCAPED form (see
    // W32EscapeName), so the jail root must be escaped too — otherwise
    // a non-ASCII jail root would falsely deny everything.
    W32EscapeName(root);
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
  abs = W32StripExtendedPrefix(abs);
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

// Extended-length Win32 paths bypass MAX_PATH without opting into the
// process-wide longPathAware policy. Call only after textual normalization
// and the jail egress check: the \\?\ prefix disables Win32 normalization.
static int W32MakeExtendedPath(wchar_t *w) {
  size_t n;
  if (W32IsDeviceName(w) || !wcsncmp(w, L"\\\\?\\", 4)) return 0;
  if (!(iswalpha(w[0]) && w[1] == L':' && w[2] == L'\\')) {
    SetLastError(ERROR_INVALID_NAME);
    return -1;
  }
  n = wcslen(w);
  if (n + 4 >= W32_PATH_MAX) {
    SetLastError(ERROR_FILENAME_EXCED_RANGE);
    return -1;
  }
  memmove(w + 4, w, (n + 1) * sizeof(*w));
  memcpy(w, L"\\\\?\\", 4 * sizeof(*w));
  return 0;
}

static wchar_t *W32Path(const char *path, wchar_t buf[W32_PATH_MAX]) {
  int n;
  wchar_t tmp[W32_PATH_MAX];
  wchar_t joined[W32_PATH_MAX];
  if (!path) return NULL;
  if (!strcmp(path, "/dev/null")) path = "NUL";
  if (path[0] == '/') {
    // absolute guest path: root at the jail root
    int root_comps;
    const wchar_t *root = W32JailRoot(&root_comps);
    if (getenv("WBOX_DEBUG_PATH"))
      fprintf(stderr, "[w32path] abs '%s'\n", path);
    n = MultiByteToWideChar(CP_UTF8, 0, path, -1, tmp, W32_PATH_MAX);
    if (n <= 0) return NULL;
    // F7/F8: escape the guest part BEFORE joining — the jail root is
    // cached in escaped form, and escaping twice would corrupt '%'.
    // "." / ".." are ASCII, so normalization below is unaffected.
    // C2: component-level normalization; reject any result that escapes
    // above the jail root (e.g. "/../outside" reaching the host).
    if (W32JoinNorm(root, tmp + 1 /* skip leading '/' */, root_comps, buf)) {
      return NULL;  // error already set
    }
  } else {
    n = MultiByteToWideChar(CP_UTF8, 0, path, -1, tmp, W32_PATH_MAX);
    if (n <= 0) {
      // retry as ANSI
      n = MultiByteToWideChar(CP_ACP, 0, path, -1, tmp, W32_PATH_MAX);
      if (n <= 0) return NULL;
    }
    // C2: cwd-relative paths may not climb above the cwd either
    if (W32NormalizeW(tmp, buf, 0)) {
      SetLastError(ERROR_ACCESS_DENIED);
      return NULL;
    }
    // F7/F8: pure-ASCII host names (locale-proof, win32-legal). Done
    // before the egress check so it compares in the same (escaped)
    // form the host filesystem sees.
    if (!W32IsDeviceName(buf)) {
      if (W32EscapeName(buf)) return NULL;
      if (buf[0] && buf[1] == L':') {
        wcscpy(joined, buf);  // already drive-prefixed absolute
      } else {
        DWORD m = GetCurrentDirectoryW(W32_PATH_MAX, joined);
        if (!m || m >= W32_PATH_MAX) return NULL;
        if (wcslen(joined) + wcslen(buf) + 2 >= W32_PATH_MAX) return NULL;
        wcscat(joined, L"\\");
        wcscat(joined, buf);
      }
      {
        wchar_t full[W32_PATH_MAX];
        if (W32NormalizeW(joined, full, 0) || W32WithinRoot(full)) {
          if (getenv("WBOX_DEBUG_PATH"))
            fprintf(stderr, "[w32path] DENY rel '%s' full '%S' root '%S'\n",
                    path, full, W32JailRoot(NULL));
          SetLastError(ERROR_ACCESS_DENIED);
          return NULL;
        }
        wcscpy(buf, full);
      }
    }
  }
  // normalize slashes to backslashes (some win32 apis insist)
  for (n = 0; buf[n]; ++n)
    if (buf[n] == L'/') buf[n] = L'\\';
  if (W32MakeExtendedPath(buf)) return NULL;
  return buf;
}

static HANDLE W32Handle(int fd);
static void W32SetAppend(int fd, int on);
static void W32SetAccess(int fd, int flags);
static int W32GetAccess(int fd);

// ------------------------------------------------ F7/F8: host filename escaping
// Wine maps Win32 filenames onto the Unix filesystem through the LOCALE
// charset — with an unset/non-UTF-8 locale any non-ASCII guest filename
// fails to create (ENOENT), and Win32 natively rejects <>:"|?* and
// control chars (EINVAL). Linux guests legitimately use both. Escape
// every such wchar as %XXXX (UTF-16 code units, '%' itself as %25) so
// host names are pure ASCII; readdir unescapes on the way back. The
// mapping is injective per code unit, so surrogate pairs round-trip
// losslessly. (Known limit: guest names ending in '.' or ' ' collide
// with win32 trailing-dot/space stripping and are not yet escaped.)
static int W32EscapeName(wchar_t *w) {
  wchar_t tmp[W32_PATH_MAX];
  wchar_t *o = tmp;
  int has_drive = iswalpha(w[0]) && w[1] == L':';
  for (size_t i = 0; w[i]; ++i) {
    wchar_t c = w[i];
    int esc = c == L'%' || c < 0x20 || c > 0x7e || c == L'<' || c == L'>' ||
              c == L'|' || c == L'?' || c == L'*' || c == L'"' ||
              (c == L':' && !(has_drive && i == 1));
    if (!esc) {
      if (o - tmp >= W32_PATH_MAX - 1) goto toolong;
      *o++ = c;
    } else {
      if (o - tmp >= W32_PATH_MAX - 5) goto toolong;
      *o++ = L'%';
      static const wchar_t hexd[] = L"0123456789abcdef";
      *o++ = hexd[(c >> 12) & 15];
      *o++ = hexd[(c >> 8) & 15];
      *o++ = hexd[(c >> 4) & 15];
      *o++ = hexd[c & 15];
    }
  }
  *o = 0;
  wcscpy(w, tmp);
  return 0;
toolong:
  SetLastError(ERROR_FILENAME_EXCED_RANGE);
  return -1;
}

static int W32JoinNorm(const wchar_t *anchor, wchar_t *rel, int floor,
                       wchar_t out[W32_PATH_MAX]) {
  wchar_t joined[W32_PATH_MAX];
  if (W32EscapeName(rel)) return -1;  // error already set
  if (wcslen(anchor) + wcslen(rel) + 3 > W32_PATH_MAX) {
    SetLastError(ERROR_FILENAME_EXCED_RANGE);
    return -1;
  }
  wcscpy(joined, anchor);
  wcscat(joined, L"\\");
  wcscat(joined, rel);
  if (W32NormalizeW(joined, out, floor)) {
    SetLastError(ERROR_ACCESS_DENIED);
    return -1;
  }
  return 0;
}

static int W32HexDig(wchar_t c) {
  return c >= L'0' && c <= L'9' ? c - L'0'
       : c >= L'a' && c <= L'f' ? c - L'a' + 10
       : c >= L'A' && c <= L'F' ? c - L'A' + 10
                                : -1;
}

// reverse of W32EscapeName (in place, only shrinks); strict %XXXX
// (4 hex digits) sequences decode, anything else passes through.
void W32UnescapeName(wchar_t *w) {
  wchar_t *r = w, *o = w;
  while (*r) {
    if (r[0] == L'%' && W32HexDig(r[1]) >= 0 && W32HexDig(r[2]) >= 0 &&
        W32HexDig(r[3]) >= 0 && W32HexDig(r[4]) >= 0) {
      *o++ = (wchar_t)((W32HexDig(r[1]) << 12) | (W32HexDig(r[2]) << 8) |
                       (W32HexDig(r[3]) << 4) | W32HexDig(r[4]));
      r += 5;
    } else {
      *o++ = *r++;
    }
  }
  *o = 0;
}

// C2 (security-audit): real dirfd semantics for the *at family. A
// relative path with dirfd != AT_FDCWD is anchored at the directory the
// dirfd handle refers to (previously dirfd was silently ignored and the
// path resolved against the process-global cwd).
static wchar_t *W32ResolveAt(int dirfd, const char *path,
                             wchar_t buf[W32_PATH_MAX]) {
  wchar_t dir[W32_PATH_MAX], rel[W32_PATH_MAX];
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
  n = GetFinalPathNameByHandleW(h, dir, W32_PATH_MAX, FILE_NAME_NORMALIZED);
  if (!n || n >= W32_PATH_MAX) return NULL;
  d = dir;
  if (!wcsncmp(d, L"\\\\?\\", 4)) d += 4;  // strip the \\?\ prefix
  if (MultiByteToWideChar(CP_UTF8, 0, path, -1, rel, W32_PATH_MAX) <= 0 &&
      MultiByteToWideChar(CP_ACP, 0, path, -1, rel, W32_PATH_MAX) <= 0) {
    return NULL;
  }
  // F7/F8: the anchor `d` comes from the host (already escaped); escape
  // the relative part before joining. "." / ".." components are ASCII
  // and unaffected, so normalization below still sees them.
  // the anchor is absolute; ".." may not climb above the drive root.
  // (the VFS/hostfs layer already clamps ".." at the guest root — this
  // is defense in depth.)
  if (W32JoinNorm(d, rel, 0, buf)) return NULL;  // error already set
  // C2 unified egress check: the resolved path must stay inside the
  // jail root, no matter which directory the dirfd points at (S3).
  if (W32WithinRoot(buf)) {
    if (getenv("WBOX_DEBUG_PATH"))
      fprintf(stderr, "[w32resolveat] DENY '%s' buf '%S' root '%S'\n", path,
              buf, W32JailRoot(NULL));
    SetLastError(ERROR_ACCESS_DENIED);
    return NULL;
  }
  if (W32MakeExtendedPath(buf)) return NULL;
  return buf;
}

// ------------------------------------------------- F1: POSIX mode emulation
// Win32 only has a read-only attribute, so the mode passed to
// open(O_CREAT)/mkdir/chmod is kept in a process-local table keyed by
// the normalized absolute wide path; W32FillStat prefers it over the
// attribute heuristic. Single-process scope (like the rest of the L2
// emulation); snapshot-fork children share it since they share the
// process.
#define W32MODE_CAP 4096
static struct {
  wchar_t *path;
  mode_t mode;
} g_modetab[W32MODE_CAP];
static SRWLOCK g_modelock = SRWLOCK_INIT;
static mode_t g_umask = 022;

static uint32_t W32ModeHash(const wchar_t *w) {
  uint32_t h = 2166136261u;
  for (; *w; ++w) {
    h ^= (uint32_t)towlower(*w);
    h *= 16777619u;
  }
  return h;
}

// absolutize an already-normalized path for use as a mode-table key
static void W32AbsKey(const wchar_t *w, wchar_t out[W32_PATH_MAX]) {
  w = W32StripExtendedPrefix(w);
  if (w[0] && w[1] == L':') {
    wcscpy(out, w);
    return;
  }
  DWORD m = GetCurrentDirectoryW(W32_PATH_MAX, out);
  if (!m || wcslen(out) + wcslen(w) + 2 >= W32_PATH_MAX) {
    wcscpy(out, w);
    return;
  }
  wcscat(out, L"\\");
  wcscat(out, w);
}

static void W32ModeSet(const wchar_t *w, mode_t mode) {
  wchar_t key[W32_PATH_MAX];
  uint32_t i, n, firstfree = UINT32_MAX;
  W32AbsKey(w, key);
  AcquireSRWLockExclusive(&g_modelock);
  i = W32ModeHash(key) & (W32MODE_CAP - 1);
  for (n = 0; n < W32MODE_CAP; ++n, i = (i + 1) & (W32MODE_CAP - 1)) {
    if (!g_modetab[i].path) {
      if (firstfree == UINT32_MAX) firstfree = i;
      continue;
    }
    if (!_wcsicmp(g_modetab[i].path, key)) {
      g_modetab[i].mode = mode;
      ReleaseSRWLockExclusive(&g_modelock);
      return;
    }
  }
  if (firstfree != UINT32_MAX &&
      (g_modetab[firstfree].path = _wcsdup(key))) {
    g_modetab[firstfree].mode = mode;
  }
  ReleaseSRWLockExclusive(&g_modelock);
}

static int W32ModeGet(const wchar_t *w, mode_t *mode) {
  wchar_t key[W32_PATH_MAX];
  uint32_t i, n;
  W32AbsKey(w, key);
  AcquireSRWLockShared(&g_modelock);
  i = W32ModeHash(key) & (W32MODE_CAP - 1);
  for (n = 0; n < W32MODE_CAP; ++n, i = (i + 1) & (W32MODE_CAP - 1)) {
    if (!g_modetab[i].path) continue;
    if (!_wcsicmp(g_modetab[i].path, key)) {
      *mode = g_modetab[i].mode;
      ReleaseSRWLockShared(&g_modelock);
      return 1;
    }
  }
  ReleaseSRWLockShared(&g_modelock);
  return 0;
}

static void W32ModeClear(const wchar_t *w) {
  wchar_t key[W32_PATH_MAX];
  uint32_t i, n;
  W32AbsKey(w, key);
  AcquireSRWLockExclusive(&g_modelock);
  i = W32ModeHash(key) & (W32MODE_CAP - 1);
  for (n = 0; n < W32MODE_CAP; ++n, i = (i + 1) & (W32MODE_CAP - 1)) {
    if (g_modetab[i].path && !_wcsicmp(g_modetab[i].path, key)) {
      free(g_modetab[i].path);
      g_modetab[i].path = NULL;
      break;
    }
  }
  ReleaseSRWLockExclusive(&g_modelock);
}

static void W32ModeMove(const wchar_t *old, const wchar_t *new_) {
  mode_t m;
  if (W32ModeGet(old, &m)) {
    W32ModeSet(new_, m);
    W32ModeClear(old);
  }
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

// mapping table centralized in w32errno.c (W32ErrFromHost)
static int W32Err(void) {
  return W32ErrFromHost(GetLastError());
}

static int W32ToCrt(HANDLE h, int flags) {
  int cflags = 0;
  if (flags & O_APPEND) cflags |= 0x0008;       // _O_APPEND
  if ((flags & O_ACCMODE) == O_RDONLY) cflags |= 0x0000;
  else if ((flags & O_ACCMODE) == O_WRONLY) cflags |= 0x0001;
  else cflags |= 0x0002;
  cflags |= 0x8000;  // _O_BINARY
  int fd = _open_osfhandle((intptr_t)h, cflags);
  if (fd == -1) {
    CloseHandle(h);
  } else {
    W32SetAccess(fd, flags);
  }
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
  wchar_t wbuf[W32_PATH_MAX];
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
    if (getenv("WBOX_DEBUG_PATH")) {
      DWORD gle = GetLastError();
      fprintf(stderr, "[openat] FAIL gle=%lu '%s' -> [", gle, path);
      for (wchar_t *q = wbuf; *q; ++q) fprintf(stderr, "%04x.", *q);
      fprintf(stderr, "]\n");
    }
    errno = W32Err();
    return -1;
  }
  // F1: mode applies only at creation (attr was INVALID before CreateFile)
  if ((flags & O_CREAT) && attr == INVALID_FILE_ATTRIBUTES) {
    W32ModeSet(wbuf, mode & ~g_umask & 07777);
  }
  if (flags & O_APPEND) SetFilePointer(h, 0, NULL, FILE_END);
  int ofd = W32ToCrt(h, flags);
  W32SetAppend(ofd, (flags & O_APPEND) != 0);
  return ofd;
}

// ---------------------------------------------------------------- io

// special fds 1000000..1000002 are fake device fds (no CRT fd)
static int IsSpecial(int fd) {
  return fd >= 1000000 && fd <= 1000002;
}

// F2: per-fd O_APPEND table. File IO here goes through bare
// WriteFile/ReadFile, which bypass the CRT's _O_APPEND handling, so the
// Linux append semantics (every write lands at EOF — even after lseek,
// and pwrite on an O_APPEND fd still appends) must be re-implemented.
// Atomicity is best-effort: seek-to-END immediately before the write.
#define W32_FDTRACK_MAX 65536
static unsigned char g_fdappend[W32_FDTRACK_MAX / 8];
// Access mode is needed by F_GETFL. MinGW's CRT doesn't expose a query for
// the mode passed to _open_osfhandle(), and reporting O_RDWR unconditionally
// makes blink treat stdin and read-only files as writable.
// Store O_ACCMODE + 1 so zero remains the "not tracked" sentinel.
static unsigned char g_fdaccess[W32_FDTRACK_MAX];
static void W32SetAppend(int fd, int on) {
  if (fd < 0 || fd >= W32_FDTRACK_MAX) return;
  if (on) {
    g_fdappend[fd >> 3] |= 1u << (fd & 7);
  } else {
    g_fdappend[fd >> 3] &= ~(1u << (fd & 7));
  }
}
static int W32IsAppend(int fd) {
  return fd >= 0 && fd < W32_FDTRACK_MAX && (g_fdappend[fd >> 3] >> (fd & 7)) & 1;
}
static void W32SetAccess(int fd, int flags) {
  if (fd < 0 || fd >= W32_FDTRACK_MAX) return;
  g_fdaccess[fd] = flags < 0 ? 0 : (flags & O_ACCMODE) + 1;
}
static int W32GetAccess(int fd) {
  if (fd >= 0 && fd < W32_FDTRACK_MAX && g_fdaccess[fd]) {
    return g_fdaccess[fd] - 1;
  }
  // Standard descriptors predate this table. Preserve their conventional
  // directions so AddStdFd() records correct guest access flags.
  if (fd == STDIN_FILENO) return O_RDONLY;
  if (fd == STDOUT_FILENO || fd == STDERR_FILENO) return O_WRONLY;
  return O_RDWR;
}

// ---------------------------------------------------------------- eventfd

#define WBOX_MAXEVENTFD 1024
#define WBOX_EFD_SEMAPHORE 1
#define WBOX_EFD_NONBLOCK O_NONBLOCK

struct WboxEventfd {
  SRWLOCK lock;
  CONDITION_VARIABLE changed;
  uint64_t counter;
  int semaphore;
  int nonblock;
  int refs;  // descriptor mappings plus in-flight operations
};

struct WboxEventfdMap {
  int used;
  int fd;
  struct WboxEventfd *obj;
};

static SRWLOCK g_eventfds_lock = SRWLOCK_INIT;
static struct WboxEventfdMap g_eventfds[WBOX_MAXEVENTFD];

static void EventfdRelease(struct WboxEventfd *);

static int EventfdSlot(int fd) {
  for (int i = 0; i < WBOX_MAXEVENTFD; ++i)
    if (g_eventfds[i].used && g_eventfds[i].fd == fd) return i;
  return -1;
}

static struct WboxEventfd *EventfdAcquire(int fd) {
  struct WboxEventfd *obj = NULL;
  AcquireSRWLockExclusive(&g_eventfds_lock);
  int i = EventfdSlot(fd);
  if (i != -1) {
    obj = g_eventfds[i].obj;
    ++obj->refs;
  }
  ReleaseSRWLockExclusive(&g_eventfds_lock);
  return obj;
}

static void EventfdRelease(struct WboxEventfd *obj) {
  int destroy = 0;
  AcquireSRWLockExclusive(&g_eventfds_lock);
  if (!--obj->refs) destroy = 1;
  ReleaseSRWLockExclusive(&g_eventfds_lock);
  if (destroy) free(obj);
}

static int EventfdMap(int fd, struct WboxEventfd *obj) {
  int slot = -1;
  AcquireSRWLockExclusive(&g_eventfds_lock);
  for (int i = 0; i < WBOX_MAXEVENTFD; ++i) {
    if (!g_eventfds[i].used) {
      slot = i;
      break;
    }
  }
  if (slot != -1) {
    g_eventfds[slot].used = 1;
    g_eventfds[slot].fd = fd;
    g_eventfds[slot].obj = obj;
    ++obj->refs;
  }
  ReleaseSRWLockExclusive(&g_eventfds_lock);
  if (slot == -1) {
    errno = EMFILE;
    return -1;
  }
  return 0;
}

static int EventfdUnmap(int fd) {
  struct WboxEventfd *obj = NULL;
  int destroy = 0;
  AcquireSRWLockExclusive(&g_eventfds_lock);
  int i = EventfdSlot(fd);
  if (i != -1) {
    obj = g_eventfds[i].obj;
    memset(&g_eventfds[i], 0, sizeof(g_eventfds[i]));
    if (!--obj->refs) destroy = 1;
  }
  ReleaseSRWLockExclusive(&g_eventfds_lock);
  if (destroy) free(obj);
  return obj != NULL;
}

int WboxEventfdCreate(unsigned int initval, int flags) {
  HANDLE h;
  int fd;
  struct WboxEventfd *obj;
  if (flags & ~(WBOX_EFD_SEMAPHORE | O_NONBLOCK | O_CLOEXEC)) {
    errno = EINVAL;
    return -1;
  }
  if (!(obj = calloc(1, sizeof(*obj)))) {
    errno = ENOMEM;
    return -1;
  }
  InitializeSRWLock(&obj->lock);
  InitializeConditionVariable(&obj->changed);
  obj->counter = initval;
  obj->semaphore = !!(flags & WBOX_EFD_SEMAPHORE);
  obj->nonblock = !!(flags & WBOX_EFD_NONBLOCK);
  // NUL is only a CRT namespace anchor; event handles are rejected by
  // wine's _open_osfhandle (same constraint as the epoll shim).
  h = CreateFileW(L"NUL", GENERIC_READ | GENERIC_WRITE,
                  FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0,
                  NULL);
  if (h == INVALID_HANDLE_VALUE) {
    free(obj);
    errno = W32Err();
    return -1;
  }
  if ((fd = W32ToCrt(h, O_RDWR)) == -1) {
    free(obj);
    errno = EMFILE;
    return -1;
  }
  if (EventfdMap(fd, obj) == -1) {
    _close(fd);
    free(obj);
    return -1;
  }
  return fd;
}

static ssize_t EventfdRead(int fd, void *buf, size_t n) {
  uint64_t value;
  struct WboxEventfd *obj;
  if (n != sizeof(value)) {
    errno = EINVAL;
    return -1;
  }
  if (!(obj = EventfdAcquire(fd))) {
    errno = EBADF;
    return -1;
  }
  AcquireSRWLockExclusive(&obj->lock);
  while (!obj->counter) {
    if (obj->nonblock) {
      ReleaseSRWLockExclusive(&obj->lock);
      EventfdRelease(obj);
      errno = EAGAIN;
      return -1;
    }
    SleepConditionVariableSRW(&obj->changed, &obj->lock, INFINITE, 0);
  }
  if (obj->semaphore) {
    value = 1;
    --obj->counter;
  } else {
    value = obj->counter;
    obj->counter = 0;
  }
  WakeAllConditionVariable(&obj->changed);
  ReleaseSRWLockExclusive(&obj->lock);
  EventfdRelease(obj);
  memcpy(buf, &value, sizeof(value));
  return sizeof(value);
}

static ssize_t EventfdWrite(int fd, const void *buf, size_t n) {
  uint64_t value;
  struct WboxEventfd *obj;
  if (n != sizeof(value)) {
    errno = EINVAL;
    return -1;
  }
  memcpy(&value, buf, sizeof(value));
  if (value == UINT64_MAX) {
    errno = EINVAL;
    return -1;
  }
  if (!(obj = EventfdAcquire(fd))) {
    errno = EBADF;
    return -1;
  }
  AcquireSRWLockExclusive(&obj->lock);
  while (value > UINT64_MAX - 1 - obj->counter) {
    if (obj->nonblock) {
      ReleaseSRWLockExclusive(&obj->lock);
      EventfdRelease(obj);
      errno = EAGAIN;
      return -1;
    }
    SleepConditionVariableSRW(&obj->changed, &obj->lock, INFINITE, 0);
  }
  obj->counter += value;
  WakeAllConditionVariable(&obj->changed);
  ReleaseSRWLockExclusive(&obj->lock);
  EventfdRelease(obj);
  return sizeof(value);
}

static int EventfdDup(int fd) {
  HANDLE h, duph;
  int nfd;
  struct WboxEventfd *obj;
  if (!(obj = EventfdAcquire(fd))) return -2;
  h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE ||
      !DuplicateHandle(GetCurrentProcess(), h, GetCurrentProcess(), &duph, 0,
                       FALSE, DUPLICATE_SAME_ACCESS)) {
    EventfdRelease(obj);
    errno = W32Err();
    return -1;
  }
  nfd = W32ToCrt(duph, O_RDWR);
  if (nfd == -1 || EventfdMap(nfd, obj) == -1) {
    if (nfd != -1) _close(nfd);
    EventfdRelease(obj);
    return -1;
  }
  EventfdRelease(obj);
  return nfd;
}

static int EventfdDup2(int fd, int fd2) {
  struct WboxEventfd *obj;
  if (!(obj = EventfdAcquire(fd))) return -2;
  if (fd == fd2) {
    EventfdRelease(obj);
    return fd2;
  }
  EventfdUnmap(fd2);
  if (_dup2(fd, fd2)) {
    EventfdRelease(obj);
    errno = EBADF;
    return -1;
  }
  if (EventfdMap(fd2, obj) == -1) {
    _close(fd2);
    EventfdRelease(obj);
    return -1;
  }
  EventfdRelease(obj);
  return fd2;
}

static int EventfdFcntl(int fd, int cmd, int arg) {
  int rc;
  struct WboxEventfd *obj;
  if (!(obj = EventfdAcquire(fd))) return -2;
  AcquireSRWLockExclusive(&obj->lock);
  switch (cmd) {
    case F_GETFL:
      rc = O_RDWR | (obj->nonblock ? O_NONBLOCK : 0);
      break;
    case F_SETFL:
      obj->nonblock = !!(arg & O_NONBLOCK);
      WakeAllConditionVariable(&obj->changed);
      rc = 0;
      break;
    case F_GETFD:
    case F_SETFD:
      rc = 0;
      break;
    default:
      rc = -2;
      break;
  }
  ReleaseSRWLockExclusive(&obj->lock);
  EventfdRelease(obj);
  return rc;
}

static int EventfdPoll(int fd, short events, short *revents) {
  struct WboxEventfd *obj;
  if (!(obj = EventfdAcquire(fd))) return 0;
  AcquireSRWLockShared(&obj->lock);
  *revents = 0;
  if (obj->counter) *revents |= events & POLLIN;
  if (obj->counter < UINT64_MAX - 1) *revents |= events & POLLOUT;
  ReleaseSRWLockShared(&obj->lock);
  EventfdRelease(obj);
  return 1;
}

static ssize_t EventfdReadv(int fd, const struct iovec *iov, int n) {
  uint64_t value;
  size_t total = 0, off = 0;
  for (int i = 0; i < n; ++i) {
    if (iov[i].iov_len > sizeof(value) - total) {
      errno = EINVAL;
      return -1;
    }
    total += iov[i].iov_len;
  }
  if (total != sizeof(value)) {
    errno = EINVAL;
    return -1;
  }
  ssize_t rc = EventfdRead(fd, &value, sizeof(value));
  if (rc == -1) return -1;
  for (int i = 0; i < n; ++i) {
    memcpy(iov[i].iov_base, (char *)&value + off, iov[i].iov_len);
    off += iov[i].iov_len;
  }
  return rc;
}

static ssize_t EventfdWritev(int fd, const struct iovec *iov, int n) {
  uint64_t value;
  size_t total = 0, off = 0;
  for (int i = 0; i < n; ++i) {
    if (iov[i].iov_len > sizeof(value) - total) {
      errno = EINVAL;
      return -1;
    }
    total += iov[i].iov_len;
  }
  if (total != sizeof(value)) {
    errno = EINVAL;
    return -1;
  }
  for (int i = 0; i < n; ++i) {
    memcpy((char *)&value + off, iov[i].iov_base, iov[i].iov_len);
    off += iov[i].iov_len;
  }
  return EventfdWrite(fd, &value, sizeof(value));
}

// Unified CRT-fd classification — THE single lookup that decides which
// backing object a host-side fd refers to (see win32.h). The namespaces are
// disjoint: specials are sentinels >= 1000000, epoll/socket are real CRT
// fds registered in the w32sock.c tables, everything else HANDLE-backed.
int W32FdClassify(int fd, struct W32FdInfo *out) {
  struct W32FdInfo tmp;
  if (!out) out = &tmp;
  out->handle = (void *)(intptr_t)-1;
  out->dev = 0;
  if (IsSpecial(fd)) {
    out->kind = W32FD_SPECIAL;
    out->dev = fd;
    return out->kind;
  }
  if (WboxEpollIsFd(fd)) {
    out->kind = W32FD_EPOLL;
    return out->kind;
  }
  struct WboxEventfd *eventfd = EventfdAcquire(fd);
  if (eventfd) {
    EventfdRelease(eventfd);
    out->kind = W32FD_EVENTFD;
    return out->kind;
  }
  if (WboxSockIsFd(fd)) {
    out->kind = W32FD_SOCKET;
    return out->kind;
  }
  HANDLE h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    out->kind = W32FD_BAD;
    return out->kind;
  }
  out->kind = W32FD_FILE;
  out->handle = (void *)h;
  return out->kind;
}

// positioned IO on pipes is ESPIPE on Linux; on win32 ReadFile with an
// OVERLAPPED offset on a pipe handle can BLOCK forever (t_fd_rw /
// t_negative empty-pipe hang), so reject before touching the handle.
static int W32EspipeIfPipe(HANDLE h) {
  if (GetFileType(h) == FILE_TYPE_PIPE) {
    errno = ESPIPE;
    return -1;
  }
  return 0;
}

// 64-bit positioned read. Saves/restores the file pointer: ReadFile
// with lpOverlapped on a synchronous handle still advances the position,
// which broke pread's "must not move the file offset" semantics (F3).
static ssize_t W32PreadAt(HANDLE h, void *buf, size_t n, uint64_t off) {
  OVERLAPPED ov;
  LARGE_INTEGER save, back;
  int have_save;
  memset(&ov, 0, sizeof(ov));
  ov.Offset = (DWORD)(off & 0xffffffffu);
  ov.OffsetHigh = (DWORD)(off >> 32);
  save.QuadPart = 0;
  have_save = SetFilePointerEx(h, save, &save, FILE_CURRENT);
  DWORD got = 0;
  BOOL ok = ReadFile(h, buf, n > 0x7fffffff ? 0x7fffffff : (DWORD)n, &got, &ov);
  DWORD e = ok ? 0 : GetLastError();
  if (have_save) SetFilePointerEx(h, save, &back, FILE_BEGIN);
  if (!ok) {
    if (e == ERROR_HANDLE_EOF) return got ? got : 0;
    errno = W32Err();
    return -1;
  }
  return got;
}

// 64-bit positioned write (same position save/restore as W32PreadAt).
static ssize_t W32PwriteAt(HANDLE h, const void *buf, size_t n, uint64_t off) {
  OVERLAPPED ov;
  LARGE_INTEGER save, back;
  int have_save;
  memset(&ov, 0, sizeof(ov));
  ov.Offset = (DWORD)(off & 0xffffffffu);
  ov.OffsetHigh = (DWORD)(off >> 32);
  save.QuadPart = 0;
  have_save = SetFilePointerEx(h, save, &save, FILE_CURRENT);
  DWORD put = 0;
  BOOL ok = WriteFile(h, buf, n > 0x7fffffff ? 0x7fffffff : (DWORD)n, &put, &ov);
  if (have_save) SetFilePointerEx(h, save, &back, FILE_BEGIN);
  if (!ok) {
    errno = W32Err();
    return -1;
  }
  return put;
}

ssize_t read(int fd, void *buf, size_t n) {
  struct W32FdInfo fi;
  switch (W32FdClassify(fd, &fi)) {
    case W32FD_SOCKET:
      return WboxSockRead(fd, buf, n);  // feat/net
    case W32FD_EVENTFD:
      return EventfdRead(fd, buf, n);
    case W32FD_SPECIAL:
      if (fi.dev == 1000001) {  // zero
        memset(buf, 0, n);
        return n;
      }
      if (fi.dev == 1000000) {  // urandom
        NTSTATUS st = BCryptGenRandom(NULL, buf, n, BCRYPT_USE_SYSTEM_PREFERRED_RNG);
        if (st != 0) {
          errno = EIO;
          return -1;
        }
        return n;
      }
      return 0;  // full: reads give eof
    case W32FD_BAD:
      errno = EBADF;
      return -1;
  }
  HANDLE h = (HANDLE)fi.handle;
  BY_HANDLE_FILE_INFORMATION hfi;
  if (GetFileInformationByHandle(h, &hfi) &&
      (hfi.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY)) {
    errno = EISDIR;
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
  struct W32FdInfo fi;
  switch (W32FdClassify(fd, &fi)) {
    case W32FD_SOCKET:
      return WboxSockWrite(fd, buf, n);  // feat/net
    case W32FD_EVENTFD:
      return EventfdWrite(fd, buf, n);
    case W32FD_SPECIAL:
      if (fi.dev == 1000002) {  // full
        errno = ENOSPC;
        return -1;
      }
      return n;
    case W32FD_BAD:
      errno = EBADF;
      return -1;
  }
  HANDLE h = (HANDLE)fi.handle;
  // F2: O_APPEND — every write goes to EOF regardless of the fd position.
  if (W32IsAppend(fd)) SetFilePointerEx(h, (LARGE_INTEGER){0}, NULL, FILE_END);
  DWORD put = 0;
  if (!WriteFile(h, buf, n > 0x7fffffff ? 0x7fffffff : (DWORD)n, &put, NULL)) {
    errno = W32Err();
    if (errno == 0) errno = EIO;
    return -1;
  }
  return put;
}

int close(int fd) {
  switch (W32FdClassify(fd, NULL)) {
    case W32FD_SOCKET:
      (void)WboxSockClose(fd);  // feat/net: socket fds (classified above)
      return 0;
    case W32FD_EPOLL:
      (void)WboxEpollClose(fd);  // feat/net: epoll fds (classified above)
      return 0;
    case W32FD_EVENTFD:
      WboxEpollPurgeFd(fd);
      EventfdUnmap(fd);
      return _close(fd) ? (errno = EBADF, -1) : 0;
    case W32FD_SPECIAL:
      return 0;
  }
  W32SetAppend(fd, 0);
  W32SetAccess(fd, -1);
  return _close(fd) ? (errno = EBADF, -1) : 0;
}

off_t lseek(int fd, off_t off, int whence) {
  if (IsSpecial(fd)) return 0;
  if (W32FdClassify(fd, NULL) == W32FD_EVENTFD) {
    errno = ESPIPE;
    return -1;
  }
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
  if (W32FdClassify(fd, NULL) == W32FD_EVENTFD) {
    errno = ESPIPE;
    return -1;
  }
  HANDLE h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return -1;
  }
  if (W32EspipeIfPipe(h)) return -1;
  return W32PreadAt(h, buf, n, (uint64_t)(uint32_t)off);
}

ssize_t pwrite(int fd, const void *buf, size_t n, off_t off) {
  if (W32FdClassify(fd, NULL) == W32FD_EVENTFD) {
    errno = ESPIPE;
    return -1;
  }
  HANDLE h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return -1;
  }
  if (W32EspipeIfPipe(h)) return -1;
  // F2: pwrite on an O_APPEND fd ignores the offset and appends (Linux).
  if (W32IsAppend(fd)) {
    LARGE_INTEGER end;
    if (!GetFileSizeEx(h, &end)) {
      errno = W32Err();
      return -1;
    }
    off = (off_t)end.QuadPart;
  }
  return W32PwriteAt(h, buf, n, (uint64_t)(uint32_t)off);
}

// F3: 64-bit-offset variants for the syscall layer. The VFS/hostfs chain
// truncates offsets to the 32-bit CRT off_t, so SysPread/SysPwrite route
// offsets beyond 2GiB here (only regular files legitimately reach such
// offsets; proc/dev pipes are rejected with ESPIPE above).
ssize_t W32Preadv64(int fd, const struct iovec *iov, int n, int64_t off) {
  if (off < 0) {
    errno = EINVAL;
    return -1;
  }
  if (IsSpecial(fd)) {
    errno = ESPIPE;
    return -1;
  }
  HANDLE h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return -1;
  }
  if (W32EspipeIfPipe(h)) return -1;
  ssize_t total = 0;
  for (int i = 0; i < n; ++i) {
    ssize_t rc = W32PreadAt(h, iov[i].iov_base, iov[i].iov_len,
                            (uint64_t)off + total);
    if (rc < 0) return total ? total : -1;
    total += rc;
    if ((size_t)rc < iov[i].iov_len) break;
  }
  return total;
}

ssize_t W32Pwritev64(int fd, const struct iovec *iov, int n, int64_t off) {
  if (off < 0) {
    errno = EINVAL;
    return -1;
  }
  HANDLE h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return -1;
  }
  if (W32EspipeIfPipe(h)) return -1;
  // F2: pwrite on an O_APPEND fd appends at EOF (Linux semantics).
  if (W32IsAppend(fd)) {
    LARGE_INTEGER end;
    if (!GetFileSizeEx(h, &end)) {
      errno = W32Err();
      return -1;
    }
    off = end.QuadPart;
  }
  ssize_t total = 0;
  for (int i = 0; i < n; ++i) {
    ssize_t rc = W32PwriteAt(h, iov[i].iov_base, iov[i].iov_len,
                             (uint64_t)off + total);
    if (rc < 0) return total ? total : -1;
    total += rc;
    if ((size_t)rc < iov[i].iov_len) break;
  }
  return total;
}

ssize_t readv(int fd, const struct iovec *iov, int n) {
  if (W32FdClassify(fd, NULL) == W32FD_EVENTFD)
    return EventfdReadv(fd, iov, n);
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
  if (W32FdClassify(fd, NULL) == W32FD_EVENTFD)
    return EventfdWritev(fd, iov, n);
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
  int eventfd = EventfdDup(fd);
  if (eventfd != -2) return eventfd;
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
  int nfd = W32ToCrt(duph, W32GetAccess(fd));
  W32SetAppend(nfd, W32IsAppend(fd));  // dup shares the file status flags
  return nfd;
}

int dup2(int fd, int fd2) {
  if (fd == fd2) return fd2;
  int eventfd = EventfdDup2(fd, fd2);
  if (eventfd != -2) return eventfd;
  EventfdUnmap(fd2);
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
    int tmp = W32ToCrt(h, O_RDWR);
    if (tmp == -1) return -1;
    if (_dup2(tmp, fd2)) {
      close(tmp);
      errno = EBADF;
      return -1;
    }
    close(tmp);
    W32SetAccess(fd2, O_RDWR);
    return fd2;
  }
  if (_dup2(fd, fd2)) {
    errno = EBADF;
    return -1;
  }
  W32SetAppend(fd2, W32IsAppend(fd));
  W32SetAccess(fd2, W32GetAccess(fd));
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
    if (fds[0] != -1) close(fds[0]);
    if (fds[1] != -1) close(fds[1]);
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
    int erc = EventfdFcntl(fd, cmd, arg);
    if (erc != -2) return erc;
    int src = WboxSockFcntl(fd, cmd, arg);
    if (src != -2) return src;
  }
  switch (cmd) {
    case F_GETFL:
      if (W32FdClassify(fd, NULL) == W32FD_BAD) {
        errno = EBADF;
        return -1;
      }
      return W32GetAccess(fd) | (W32IsAppend(fd) ? O_APPEND : 0);
    case F_SETFL:
      W32SetAppend(fd, (arg & O_APPEND) != 0);
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
  // F1: the emulated POSIX mode table wins over the attribute heuristic
  if (wpath && (ft == FILE_TYPE_DISK)) {
    mode_t em;
    if (W32ModeGet(wpath, &em)) {
      st->st_mode = (st->st_mode & S_IFMT) | (em & 07777);
    }
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

// F3: struct stat's st_size is the 32-bit CRT off_t on win32, so files
// >=2GiB report truncated sizes. The syscall layer uses this to recover
// the true 64-bit size after VfsFstat. Fails (returns -1) for handles
// without a size (pipes, consoles) — callers keep the 32-bit value then.
int W32FileSize64(int fd, int64_t *out) {
  HANDLE h = W32Handle(fd);
  LARGE_INTEGER sz;
  if (h == INVALID_HANDLE_VALUE || !GetFileSizeEx(h, &sz)) return -1;
  *out = sz.QuadPart;
  return 0;
}

int fstat(int fd, struct stat *st) {
  struct W32FdInfo fi;
  switch (W32FdClassify(fd, &fi)) {
    case W32FD_SOCKET: {  // feat/net
      unsigned sockmode = 0;
      (void)WboxSockFstatMode(fd, &sockmode);  // classified above
      memset(st, 0, sizeof(*st));
      st->st_mode = sockmode;
      st->st_blksize = 4096;
      st->st_nlink = 1;
      return 0;
    }
    case W32FD_EPOLL:  // feat/net
      memset(st, 0, sizeof(*st));
      st->st_mode = S_IFCHR | 0600;
      st->st_blksize = 4096;
      st->st_nlink = 1;
      return 0;
    case W32FD_EVENTFD:
      memset(st, 0, sizeof(*st));
      st->st_mode = S_IFCHR | 0600;
      st->st_blksize = 4096;
      st->st_nlink = 1;
      return 0;
    case W32FD_SPECIAL:
      memset(st, 0, sizeof(*st));
      st->st_mode = S_IFCHR | 0666;
      st->st_blksize = 4096;
      return 0;
    case W32FD_BAD:
      errno = EBADF;
      return -1;
  }
  HANDLE h = (HANDLE)fi.handle;
  // F1: resolve the path for the emulated-mode lookup (fails for pipes)
  wchar_t wbuf[W32_PATH_MAX], *wp = NULL;
  DWORD wn = GetFinalPathNameByHandleW(h, wbuf, W32_PATH_MAX, FILE_NAME_NORMALIZED);
  if (wn && wn < W32_PATH_MAX) {
    wp = wbuf;
    if (!wcsncmp(wp, L"\\\\?\\", 4)) wp += 4;
  }
  W32FillStat(st, h, wp);
  return 0;
}

int stat(const char *path, struct stat *st) {
  return fstatat(AT_FDCWD, path, st, 0);
}

int lstat(const char *path, struct stat *st) {
  return fstatat(AT_FDCWD, path, st, AT_SYMLINK_NOFOLLOW);
}

int fstatat(int dirfd, const char *path, struct stat *st, int flags) {
  wchar_t wbuf[W32_PATH_MAX];
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
  wchar_t wbuf[W32_PATH_MAX];
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
  return unlinkat(AT_FDCWD, path, 0);
}

// F5: POSIX unlink of an open file. DeleteFile on Win32/wine only marks
// the file delete-pending — the name stays visible (stat succeeds) until
// the last handle closes, violating POSIX unlink semantics. Emulation:
// rename the victim to a hidden sibling (open handles stay valid and
// keep the inode), then delete the temp name; its delete-pending residue
// disappears at the last close and can only ever show up as a hidden
// dotfile in readdir in between.
static int W32UnlinkPosix(const wchar_t *wbuf) {
  wchar_t tmp[W32_PATH_MAX];
  wchar_t *base;
  static volatile long seq;
  if (DeleteFileW(wbuf) &&
      GetFileAttributesW(wbuf) == INVALID_FILE_ATTRIBUTES) {
    return 0;  // not open anywhere: plain delete removed the name
  }
  if (wcslen(wbuf) + 40 >= W32_PATH_MAX) {
    SetLastError(ERROR_FILENAME_EXCED_RANGE);
    return -1;
  }
  wcscpy(tmp, wbuf);
  base = wcsrchr(tmp, L'\\');
  base = base ? base + 1 : tmp;
  swprintf(base, (size_t)(tmp + W32_PATH_MAX - base), L".wbox-unlink-%lx-%lx",
           (unsigned long)GetCurrentProcessId(),
           (unsigned long)InterlockedIncrement(&seq));
  if (!MoveFileExW(wbuf, tmp, MOVEFILE_REPLACE_EXISTING)) {
    return -1;  // original DeleteFile error is the better errno; restore
  }
  DeleteFileW(tmp);  // may go delete-pending; freed at last close
  return 0;
}

int unlinkat(int dirfd, const char *path, int flags) {
  wchar_t wbuf[W32_PATH_MAX];
  DWORD attr;
  if (!W32ResolveAt(dirfd, path, wbuf)) {
    errno = ENOENT;
    return -1;
  }
  if (flags & AT_REMOVEDIR) {
    if (!RemoveDirectoryW(wbuf)) {
      errno = W32Err();
      return -1;
    }
    W32ModeClear(wbuf);
    return 0;
  }
  // F6: Linux reports EISDIR for unlink(dir), not EACCES
  attr = GetFileAttributesW(wbuf);
  if (attr != INVALID_FILE_ATTRIBUTES && (attr & FILE_ATTRIBUTE_DIRECTORY)) {
    errno = EISDIR;
    return -1;
  }
  if (W32UnlinkPosix(wbuf)) {
    errno = W32Err();
    return -1;
  }
  W32ModeClear(wbuf);
  return 0;
}

int rmdir(const char *path) {
  wchar_t wbuf[W32_PATH_MAX];
  if (!W32Path(path, wbuf) || !RemoveDirectoryW(wbuf)) {
    errno = W32Err();
    return -1;
  }
  return 0;
}

int rename(const char *old, const char *new_) {
  wchar_t wo[W32_PATH_MAX], wn[W32_PATH_MAX];
  if (!W32Path(old, wo) || !W32Path(new_, wn)) {
    errno = ENOENT;
    return -1;
  }
  if (!MoveFileExW(wo, wn, MOVEFILE_REPLACE_EXISTING | MOVEFILE_COPY_ALLOWED)) {
    errno = W32Err();
    return -1;
  }
  W32ModeMove(wo, wn);
  return 0;
}

int renameat(int oldfd, const char *old, int newfd, const char *new_) {
  wchar_t wo[W32_PATH_MAX], wn[W32_PATH_MAX];
  if (!W32ResolveAt(oldfd, old, wo) || !W32ResolveAt(newfd, new_, wn)) {
    errno = ENOENT;
    return -1;
  }
  if (!MoveFileExW(wo, wn, MOVEFILE_REPLACE_EXISTING | MOVEFILE_COPY_ALLOWED)) {
    errno = W32Err();
    return -1;
  }
  W32ModeMove(wo, wn);
  return 0;
}

int renameat2(int oldfd, const char *old, int newfd, const char *new_,
              unsigned flags) {
  return renameat(oldfd, old, newfd, new_);  // RENAME_NOREPLACE etc. unimpl
}

int mkdir(const char *path, mode_t mode) {
  wchar_t wbuf[W32_PATH_MAX];
  if (!W32Path(path, wbuf)) {
    errno = ENOENT;
    return -1;
  }
  if (!CreateDirectoryW(wbuf, NULL)) {
    errno = W32Err();
    return -1;
  }
  W32ModeSet(wbuf, mode & ~g_umask & 07777);
  return 0;
}

int mkdirat(int dirfd, const char *path, mode_t mode) {
  wchar_t wbuf[W32_PATH_MAX];
  if (!W32ResolveAt(dirfd, path, wbuf)) {
    errno = ENOENT;
    return -1;
  }
  if (!CreateDirectoryW(wbuf, NULL)) {
    errno = W32Err();
    return -1;
  }
  W32ModeSet(wbuf, mode & ~g_umask & 07777);
  return 0;
}

int chmod(const char *path, mode_t mode) {
  wchar_t wbuf[W32_PATH_MAX];
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
  W32ModeSet(wbuf, mode & 07777);
  return 0;
}

int fchmod(int fd, mode_t mode) {
  // F1: attribute emulation only covers the read-only bit; record the
  // full mode in the emulation table so fstat reports it.
  wchar_t wbuf[W32_PATH_MAX];
  HANDLE h = W32Handle(fd);
  if (h == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return -1;
  }
  DWORD n = GetFinalPathNameByHandleW(h, wbuf, W32_PATH_MAX, FILE_NAME_NORMALIZED);
  if (n && n < W32_PATH_MAX) {
    wchar_t *d = wbuf;
    if (!wcsncmp(d, L"\\\\?\\", 4)) d += 4;
    W32ModeSet(d, mode & 07777);
  }
  return 0;
}

int fchmodat(int dirfd, const char *path, mode_t mode, int flags) {
  wchar_t wbuf[W32_PATH_MAX];
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
  W32ModeSet(wbuf, mode & 07777);
  return 0;
}

mode_t umask(mode_t m) {
  mode_t old = g_umask;
  g_umask = m & 0777;
  return old;
}

int link(const char *old, const char *new_) {
  wchar_t wo[W32_PATH_MAX], wn[W32_PATH_MAX];
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
  wchar_t wo[W32_PATH_MAX], wn[W32_PATH_MAX];
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
  wchar_t wbuf[W32_PATH_MAX];
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
  wchar_t wbuf[W32_PATH_MAX];
  DWORD n = GetCurrentDirectoryW(W32_PATH_MAX, wbuf);
  if (!n) {
    errno = W32Err();
    return NULL;
  }
  // convert backslashes and prepend drive-relative root:
  // wine cwd like Z:\tmp\wbox5 -> /tmp/wbox5 (drop drive letter)
  wchar_t *p = wbuf;
  p = (wchar_t *)W32StripExtendedPrefix(p);
  if (p[0] && p[1] == L':') p += 2;
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

// THE shared wait primitive of the win32 port: poll/epoll_wait and (via
// fd->cb->poll through the VFS) syscall.c's W32NonblockPoll gate all funnel
// here. The per-class readiness semantics live in exactly one place:
// sockets are level-triggered WSAPoll (w32sock.c, sliced so the non-socket
// fds are re-checked regularly), files/consoles are always ready, pipes are
// PeekNamedPipe'd with a handle-wait fallback for wine fifos.
int W32WaitFds(struct pollfd *pfds, nfds_t n, int timeout) {
  DWORD deadline = timeout >= 0 ? GetTickCount() + timeout : 0;
  int nsock = 0;
  for (nfds_t i = 0; i < n; ++i)
    if (pfds[i].fd >= 0 && W32FdClassify(pfds[i].fd, NULL) == W32FD_SOCKET)
      ++nsock;
  for (;;) {
    int nready = 0;
    for (nfds_t i = 0; i < n; ++i) {
      struct pollfd *p = pfds + i;
      struct W32FdInfo fi;
      p->revents = 0;
      if (p->fd < 0) continue;
      switch (W32FdClassify(p->fd, &fi)) {
        case W32FD_SOCKET:
          continue;  // serviced by WboxSockPoll below
        case W32FD_EVENTFD:
          EventfdPoll(p->fd, p->events, &p->revents);
          break;
        case W32FD_SPECIAL:
          p->revents = p->events & (POLLIN | POLLOUT);
          break;
        case W32FD_BAD:
          p->revents = POLLNVAL;
          break;
        default: {
          HANDLE h = (HANDLE)fi.handle;
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
          break;
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

int poll(struct pollfd *pfds, nfds_t n, int timeout) {
  return W32WaitFds(pfds, n, timeout);
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
