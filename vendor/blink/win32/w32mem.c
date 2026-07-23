// wbox win32: guest address space manager
// Implements POSIX mmap/munmap/mprotect/msync over a single large
// VirtualAlloc reservation with self-managed commit/decommit, replacing
// the unix mmap MAP_FIXED / partial-munmap semantics blink relies on.
#include <errno.h>
#include <stdint.h>
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
uint64_t kSkew;

static uintptr_t g_base;      // reservation base (0 until init)
static uintptr_t g_limit;     // reservation end
static SRWLOCK g_lock = SRWLOCK_INIT;
static uintptr_t g_hinttop;   // bump allocator for hintless anon maps

// committed interval table, sorted by start
struct Iv {
  uintptr_t a, b;
};
static struct Iv *g_iv;
static size_t g_ivn, g_ivcap;

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

// Reserve the guest address space window [kSkew, kSkew+size).
// Strategy (wine-safe): ask the OS to pick the base by passing a NULL
// address to VirtualAlloc (same mechanism as NtAllocateVirtualMemory
// with ZeroBits), then adopt that base as the runtime kSkew. Probing
// fixed TB-sized addresses loops / gets OOM-killed under Wine 8.0.
// Tries 43 address bits (8TB) first, shrinking on failure; only if
// system-chosen bases keep failing do we fall back to a fixed hint.
// NB: starting higher than 43 is dangerous under Wine: a >=16TB
// MEM_RESERVE gets the process SIGKILLed (not a clean failure).
int WboxMemInit(void) {
  int bits;
  if (g_base) return 0;
  for (bits = 43; bits >= 40; --bits) {
    uint64_t size = (1ULL << bits) - 0x10000;
    void *p = VirtualAlloc(NULL, size, MEM_RESERVE, PAGE_NOACCESS);
    if (p) {
      g_base = (uintptr_t)p;
      goto ok;
    }
  }
  // fallback: fixed-address attempt at 16TB, shrinking
  for (bits = 43; bits >= 40; --bits) {
    uint64_t size = (1ULL << bits) - 0x10000;
    void *p = VirtualAlloc((LPVOID)(uintptr_t)0x1000000000000ULL, size,
                           MEM_RESERVE, PAGE_NOACCESS);
    if (p) {
      g_base = (uintptr_t)p;
      goto ok;
    }
  }
  return -1;
ok:
  kSkew = (uint64_t)g_base;
  if (getenv("WBOX_DEBUG_MEM"))
    fprintf(stderr, "dbg meminit base=%p limit=%p bits=%d\n", (void *)g_base,
            (void *)g_limit, bits);
  g_limit = g_base + ((1ULL << bits) - 0x10000);
  g_vabits = bits;
  g_hinttop = g_base + 0x10000000;  // 256MB: above typical images
  return 0;
}

uintptr_t WboxMemLimit(void) {
  return g_limit;
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
  ReleaseSRWLockExclusive(&g_lock);
  // file-backed: populate by reading (MAP_PRIVATE copy semantics)
  if (!(flags & MAP_ANONYMOUS) && fd >= 0) {
    size_t done = 0;
    unsigned char *p = (unsigned char *)r;
    if (prot & PROT_WRITE) {
      // need writable to populate; assume RW already or reprotect
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
