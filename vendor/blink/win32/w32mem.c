// wbox win32: guest address space manager
// Implements POSIX mmap/munmap/mprotect/msync over a single large
// VirtualAlloc reservation with self-managed commit/decommit, replacing
// the unix mmap MAP_FIXED / partial-munmap semantics blink relies on.
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#include <windows.h>

#include "blink/tunables.h"
#include "win32.h"

#define W32PAGE 4096ULL

// runtime guest->host skew (base of the reserved guest VA window).
// Declared extern in blink/tunables.h for the win32 build.
// THREAD-LOCAL since the fork work: each guest "process" (vfork child
// after execve) gets its own VA window, so address translation must use
// the window of the thread that owns the current guest System.
_Thread_local uint64_t kSkew;

// committed interval table, sorted by start
struct Iv {
  uintptr_t a, b;
};

// Per-window allocator state. Guest address spaces of different Systems
// must not collide in host VA, so each one reserves its own window and
// all commit/decommit bookkeeping is per window. The "current" window is
// a thread-local pointer: syscall handlers run on the owning guest
// thread, so allocator calls naturally apply to the right window.
struct WboxWindow {
  uintptr_t base;       // reservation base (0 until init)
  uintptr_t limit;      // reservation end
  uintptr_t hinttop;    // bump allocator for hintless anon maps
  int bits;             // address bits reserved
  SRWLOCK lock;
  struct Iv *iv;        // committed interval table, sorted by start
  size_t ivn, ivcap;
  int index;            // slot in g_windows
};

#define WBOX_MAX_WINDOWS 64
static struct WboxWindow *g_windows[WBOX_MAX_WINDOWS];
static SRWLOCK g_windows_lock = SRWLOCK_INIT;
static _Thread_local struct WboxWindow *g_win;

#define g_iv (g_win->iv)
#define g_ivn (g_win->ivn)
#define g_ivcap (g_win->ivcap)
#define g_base (g_win->base)
#define g_limit (g_win->limit)
#define g_lock (g_win->lock)
#define g_hinttop (g_win->hinttop)

static int IvInsert(uintptr_t a, uintptr_t b) {
  size_t i = 0;
  if (g_ivn == g_ivcap) {
    size_t nc = g_ivcap ? g_ivcap * 2 : 256;
    struct Iv *ni = realloc(g_iv, nc * sizeof(*ni));
    if (!ni) return -1;
    g_iv = ni;
    g_ivcap = nc;
  }
  while (i < g_ivn && g_iv[i].a < a) ++i;
  memmove(g_iv + i + 1, g_iv + i, (g_ivn - i) * sizeof(*g_iv));
  g_iv[i].a = a;
  g_iv[i].b = b;
  ++g_ivn;
  // merge with neighbors
  while (i + 1 < g_ivn && g_iv[i].b >= g_iv[i + 1].a) {
    if (g_iv[i + 1].b > g_iv[i].b) g_iv[i].b = g_iv[i + 1].b;
    memmove(g_iv + i + 1, g_iv + i + 2, (g_ivn - i - 2) * sizeof(*g_iv));
    --g_ivn;
  }
  if (i > 0 && g_iv[i - 1].b >= g_iv[i].a) {
    if (g_iv[i].b > g_iv[i - 1].b) g_iv[i - 1].b = g_iv[i].b;
    memmove(g_iv + i, g_iv + i + 1, (g_ivn - i - 1) * sizeof(*g_iv));
    --g_ivn;
  }
  return 0;
}

static void IvRemove(uintptr_t a, uintptr_t b) {
  size_t i;
  for (i = 0; i < g_ivn;) {
    uintptr_t s = g_iv[i].a, e = g_iv[i].b;
    if (e <= a) {
      ++i;
      continue;
    }
    if (s >= b) break;
    if (s < a && e > b) {
      // split
      if (g_ivn == g_ivcap) {
        size_t nc = g_ivcap * 2;
        struct Iv *ni = realloc(g_iv, nc * sizeof(*ni));
        if (!ni) return;  // can't happen: removal after insert ensured cap
        g_iv = ni;
        g_ivcap = nc;
      }
      memmove(g_iv + i + 1, g_iv + i, (g_ivn - i) * sizeof(*g_iv));
      ++g_ivn;
      g_iv[i].b = a;
      g_iv[i + 1].a = b;
      return;
    }
    if (s < a) {
      g_iv[i].b = a;
      ++i;
      continue;
    }
    if (e > b) {
      g_iv[i].a = b;
      break;
    }
    memmove(g_iv + i, g_iv + i + 1, (g_ivn - i - 1) * sizeof(*g_iv));
    --g_ivn;
  }
}

// is any page of [a,b) committed?
static int IvOverlaps(uintptr_t a, uintptr_t b) {
  size_t i;
  for (i = 0; i < g_ivn; ++i) {
    if (g_iv[i].b <= a) continue;
    if (g_iv[i].a >= b) break;
    return 1;
  }
  return 0;
}

static DWORD W32Prot(int prot) {
  if (prot & PROT_EXEC) {
    if (prot & PROT_WRITE) return PAGE_EXECUTE_READWRITE;
    if (prot & PROT_READ) return PAGE_EXECUTE_READ;
    return PAGE_EXECUTE;
  }
  if (prot & PROT_WRITE) return PAGE_READWRITE;
  if (prot & PROT_READ) return PAGE_READONLY;
  return PAGE_NOACCESS;
}

static int g_vabits;
int WboxMemVabits(void) {
  return g_vabits;
}

// Reserve a guest address space window [base, base+size).
// Strategy (wine-safe): ask the OS to pick the base by passing a NULL
// address to VirtualAlloc (same mechanism as NtAllocateVirtualMemory
// with ZeroBits). Probing fixed TB-sized addresses loops / gets
// OOM-killed under Wine 8.0. Tries `maxbits` address bits first,
// shrinking on failure; only if system-chosen bases keep failing do we
// fall back to a fixed hint.
// NB: starting higher than 43 is dangerous under Wine: a >=16TB
// MEM_RESERVE gets the process SIGKILLed (not a clean failure).
static struct WboxWindow *WboxWindowReserve(int maxbits) {
  int bits, slot;
  uintptr_t base = 0;
  struct WboxWindow *w;
  for (bits = maxbits; bits >= 40; --bits) {
    uint64_t size = (1ULL << bits) - 0x10000;
    void *p = VirtualAlloc(NULL, size, MEM_RESERVE, PAGE_NOACCESS);
    if (p) {
      base = (uintptr_t)p;
      goto ok;
    }
  }
  // fallback: fixed-address attempt at 16TB, shrinking
  for (bits = maxbits; bits >= 40; --bits) {
    uint64_t size = (1ULL << bits) - 0x10000;
    void *p = VirtualAlloc((LPVOID)(uintptr_t)0x1000000000000ULL, size,
                           MEM_RESERVE, PAGE_NOACCESS);
    if (p) {
      base = (uintptr_t)p;
      goto ok;
    }
  }
  return 0;
ok:
  if (!(w = calloc(1, sizeof(*w)))) {
    VirtualFree((LPVOID)base, 0, MEM_RELEASE);
    return 0;
  }
  AcquireSRWLockExclusive(&g_windows_lock);
  for (slot = 0; slot < WBOX_MAX_WINDOWS && g_windows[slot]; ++slot)
    ;
  if (slot == WBOX_MAX_WINDOWS) {
    ReleaseSRWLockExclusive(&g_windows_lock);
    VirtualFree((LPVOID)base, 0, MEM_RELEASE);
    free(w);
    return 0;
  }
  g_windows[slot] = w;
  ReleaseSRWLockExclusive(&g_windows_lock);
  w->index = slot;
  w->base = base;
  w->limit = base + ((1ULL << bits) - 0x10000);
  w->bits = bits;
  InitializeSRWLock(&w->lock);
  w->hinttop = base + 0x10000000;  // 256MB: above typical images
  if (getenv("WBOX_DEBUG_MEM"))
    fprintf(stderr, "wbox mem: window #%d [%p,%p) bits=%d\n", slot,
            (void *)w->base, (void *)w->limit, bits);
  return w;
}

int WboxMemInit(void) {
  struct WboxWindow *w;
  if (g_windows[0]) return 0;
  // feat/listener: cap the guest window at 40 bits (1TB). Under wine 11.11,
  // an N-byte MEM_RESERVE costs an extra N/4096-byte committed+zeroed
  // anonymous region (one status byte per reserved page): 43 bits cost ~2GB
  // RSS per wbox process, so two wbox processes exceeded the 3GiB CI memcg
  // and the OOM killer SIGKILLed the listener (clients then saw
  // ECONNREFUSED). 40 bits costs ~256MB. Real Windows has no such overhead.
  if (!(w = WboxWindowReserve(40))) return -1;
  g_win = w;
  kSkew = (uint64_t)w->base;
  g_vabits = w->bits;
  return 0;
}

uintptr_t WboxMemLimit(void) {
  return g_limit;
}

// ---- multi-window support for vfork-style fork (see WIN32-PORT.md) ----

// Opaque handle of the calling thread's current window, for handing to a
// freshly spawned child thread that must share this address space.
void *WboxMemCurrentWindow(void) {
  return g_win;
}

// Base address of the calling thread's current window.
uintptr_t WboxMemWindowBase(void) {
  return g_win ? g_win->base : 0;
}

// Adopt a window handle obtained from WboxMemCurrentWindow() on another
// thread. Must be called before any guest memory access on this thread.
void WboxMemSetWindow(void *win) {
  g_win = (struct WboxWindow *)win;
  kSkew = (uint64_t)g_win->base;
}

// Reserve a fresh, empty guest VA window and make it current for the
// calling thread. Used by a vfork child at execve() time: the child's
// new System is loaded into its own window so the parent's address
// space (same guest addresses, different window) stays intact.
int WboxMemForkWindow(void) {
  struct WboxWindow *w;
  if (!(w = WboxWindowReserve(g_vabits))) {
    errno = ENOMEM;
    return -1;
  }
  g_win = w;
  kSkew = (uint64_t)w->base;
  return 0;
}

// Release the calling thread's window (must not be window #0) and switch
// back to the primary window. Called when an exec'd vfork child exits.
// Caller must have unmapped all guest pages already (FreeSystem does).
// NB: recycled host pages handed out from this window are purged by the
// caller via WboxPurgeHostPagesInRange() before the reservation goes away.
void WboxMemReleaseWindow(void) {
  struct WboxWindow *w = g_win;
  if (!w || w->index == 0) return;
  AcquireSRWLockExclusive(&g_windows_lock);
  g_windows[w->index] = 0;
  ReleaseSRWLockExclusive(&g_windows_lock);
  g_win = g_windows[0];
  kSkew = g_win ? (uint64_t)g_win->base : 0;
  VirtualFree((LPVOID)w->base, 0, MEM_RELEASE);
  free(w->iv);
  free(w);
}

static void *W32Commit(uintptr_t a, size_t len, int prot) {
  if (!VirtualAlloc((LPVOID)a, len, MEM_COMMIT, W32Prot(prot))) {
    return MAP_FAILED;
  }
  if (IvInsert(a, a + len) == -1) {
    VirtualFree((LPVOID)a, len, MEM_DECOMMIT);
    errno = ENOMEM;
    return MAP_FAILED;
  }
  return (void *)a;
}

// find a free hole of len bytes starting from hint (0 = use bump top)
static uintptr_t W32FindHole(uintptr_t hint, size_t len) {
  uintptr_t p;
  size_t i;
  if (hint < g_base) hint = 0;
  if (hint) {
    // honor locality hint if free
    p = (hint + W32PAGE - 1) & ~(W32PAGE - 1);
    if (p >= g_base && p + len <= g_limit && !IvOverlaps(p, p + len)) {
      return p;
    }
  }
  // bump allocator with hole check
  p = g_hinttop;
  for (i = 0; i < g_ivn; ++i) {
    if (g_iv[i].b <= p) continue;
    if (g_iv[i].a >= p + len) break;
    p = g_iv[i].b;
  }
  if (p + len > g_limit) return 0;
  g_hinttop = p + len;
  return p;
}

void *mmap(void *addr, size_t len, int prot, int flags, int fd, off_t off) {
  uintptr_t a;
  if (!len) {
    errno = EINVAL;
    return MAP_FAILED;
  }
  len = (len + W32PAGE - 1) & ~(W32PAGE - 1);
  if ((uintptr_t)addr & (W32PAGE - 1)) {
    errno = EINVAL;
    return MAP_FAILED;
  }
  AcquireSRWLockExclusive(&g_lock);
  if (flags & MAP_FIXED) {
    a = (uintptr_t)addr;
    if (a < g_base || a + len > g_limit) {
      ReleaseSRWLockExclusive(&g_lock);
      errno = ENOMEM;
      return MAP_FAILED;
    }
    // clobber: decommit any overlapping committed pages
    if (IvOverlaps(a, a + len)) {
      IvRemove(a, a + len);
      VirtualFree((LPVOID)a, len, MEM_DECOMMIT);
    }
  } else if (flags & MAP_FIXED_NOREPLACE) {
    a = (uintptr_t)addr;
    if (a < g_base || a + len > g_limit || IvOverlaps(a, a + len)) {
      ReleaseSRWLockExclusive(&g_lock);
      errno = EEXIST;
      return MAP_FAILED;
    }
  } else {
    a = W32FindHole((uintptr_t)addr, len);
    if (!a) {
      ReleaseSRWLockExclusive(&g_lock);
      errno = ENOMEM;
      return MAP_FAILED;
    }
  }
  void *r = W32Commit(a, len, prot);
  if (r == MAP_FAILED) {
    ReleaseSRWLockExclusive(&g_lock);
    errno = ENOMEM;
    return MAP_FAILED;
  }
  if (getenv("WBOX_DEBUG_MEM"))
    fprintf(stderr, "wbox mmap: a=%p len=%#zx prot=%d flags=%#x fd=%d off=%lld\n",
            (void *)a, len, prot, flags, fd, (long long)off);
  ReleaseSRWLockExclusive(&g_lock);
  // file-backed: populate by reading (MAP_PRIVATE copy semantics)
  if (!(flags & MAP_ANONYMOUS) && fd >= 0) {
    size_t done = 0;
    unsigned char *p = (unsigned char *)r;
    // pages were committed with the final protection; pread() needs them
    // writable, so always drop to RW first and restore afterwards.
    if (!(prot & PROT_WRITE)) {
      DWORD old;
      VirtualProtect((LPVOID)r, len, PAGE_READWRITE, &old);
    }
    while (done < len) {
      ssize_t rc = pread(fd, p + done, len - done, off + (off_t)done);
      if (rc <= 0) break;
      done += (size_t)rc;
    }
    if (!(prot & PROT_WRITE)) {
      DWORD old;
      VirtualProtect((LPVOID)r, len, W32Prot(prot), &old);
    }
    // NB: MAP_SHARED writeback not implemented (L1 gap)
  }
  return r;
}

int munmap(void *addr, size_t len) {
  uintptr_t a = (uintptr_t)addr;
  if (!len) {
    errno = EINVAL;
    return -1;
  }
  len = (len + W32PAGE - 1) & ~(W32PAGE - 1);
  AcquireSRWLockExclusive(&g_lock);
  IvRemove(a, a + len);
  // decommit subrange; ignore failure (may straddle uncommitted)
  uintptr_t p;
  for (p = a; p < a + len; p += 0x1000) {
    MEMORY_BASIC_INFORMATION mbi;
    if (VirtualQuery((LPVOID)p, &mbi, sizeof(mbi)) &&
        mbi.State == MEM_COMMIT) {
      uintptr_t s = (uintptr_t)mbi.BaseAddress;
      uintptr_t e = s + mbi.RegionSize;
      if (s < a) s = a;
      if (e > a + len) e = a + len;
      VirtualFree((LPVOID)s, e - s, MEM_DECOMMIT);
      p = (e - 1) & ~(W32PAGE - 1);
    }
  }
  ReleaseSRWLockExclusive(&g_lock);
  return 0;
}

int mprotect(void *addr, size_t len, int prot) {
  DWORD old;
  if (!len) return 0;
  if (!VirtualProtect((LPVOID)addr, len, W32Prot(prot), &old)) {
    errno = EINVAL;
    return -1;
  }
  return 0;
}

int msync(void *addr, size_t len, int flags) {
  MEMORY_BASIC_INFORMATION mbi;
  if (!VirtualQuery((LPVOID)addr, &mbi, sizeof(mbi)) ||
      mbi.State != MEM_COMMIT) {
    errno = ENOMEM;
    return -1;
  }
  return 0;
}

int madvise(void *addr, size_t len, int advice) {
  return 0;
}

int mlock(const void *addr, size_t len) {
  return VirtualLock((LPVOID)addr, len) ? 0 : (errno = ENOMEM, -1);
}

int munlock(const void *addr, size_t len) {
  return VirtualUnlock((LPVOID)addr, len) ? 0 : (errno = ENOMEM, -1);
}

void *mremap(void *old, size_t oldlen, size_t newlen, int flags, ...) {
  // L1: only support shrink or fail; busybox rarely remaps
  errno = ENOMEM;
  return MAP_FAILED;
}
