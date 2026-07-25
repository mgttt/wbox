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

// H7 (security-audit): range validation helper. All length arithmetic
// below must go through this so a guest-controlled (or blink-internal)
// near-2^64 length can not wrap a+len back into the window and make
// commit/decommit/protect operate on the wrong host pages.
// Requires: caller holds or will hold the window lock; g_win != NULL.
static int W32RangeOk(uintptr_t a, size_t len) {
  return len > 0 && a >= g_base && a <= g_limit && len <= g_limit - a;
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
  if (maxbits > 43) maxbits = 43;
  if (maxbits < 38) maxbits = 38;
  for (bits = maxbits; bits >= 38; --bits) {
    uint64_t size = (1ULL << bits) - 0x10000;
    void *p = VirtualAlloc(NULL, size, MEM_RESERVE, PAGE_NOACCESS);
    if (p) {
      base = (uintptr_t)p;
      goto ok;
    }
  }
  // fallback: fixed-address attempt at 16TB, shrinking
  for (bits = maxbits; bits >= 38; --bits) {
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
  int bits = 40;
  const char *e;
  if (g_windows[0]) return 0;
  // feat/listener: cap the guest window at 40 bits (1TB). Under wine 11.11,
  // an N-byte MEM_RESERVE costs an extra N/4096-byte committed+zeroed
  // anonymous region (one status byte per reserved page): 43 bits cost ~2GB
  // RSS per wbox process, so two wbox processes exceeded the 3GiB CI memcg
  // and the OOM killer SIGKILLed the listener (clients then saw
  // ECONNREFUSED). 40 bits costs ~256MB. Real Windows has no such overhead.
  // feat/cow: WBOX_VA_BITS (38..43) overrides the default for experiments;
  // snapshot fork reserves one window per live guest process, so each extra
  // bit doubles the per-process phantom RSS under wine.
  if ((e = getenv("WBOX_VA_BITS"))) {
    int v = atoi(e);
    if (v >= 38 && v <= 43) bits = v;
  }
  if (!(w = WboxWindowReserve(bits))) return -1;
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

// Destroy an ARBITRARY window (not necessarily the calling thread's
// current one): detach, decommit everything, release the reservation,
// free the bookkeeping. Used by the W32Fork failure paths to not leak
// the 1TB snapshot window when child setup fails after the copy, and by
// WboxMemReleaseWindow for the normal exit path.
void WboxMemDestroyWindow(void *win) {
  struct WboxWindow *w = (struct WboxWindow *)win;
  size_t i;
  if (!w || w->index == 0) return;
  // H5 (security-audit): detach from the global table first, then take
  // the window lock exclusively before touching the interval table and
  // freeing — a concurrent WboxMemSnapshotWindow holds src->lock shared
  // while reading the same table, and WboxMemRecommitIfOurs takes it
  // exclusive. Releasing the reservation without the lock raced both.
  AcquireSRWLockExclusive(&g_windows_lock);
  g_windows[w->index] = 0;
  ReleaseSRWLockExclusive(&g_windows_lock);
  AcquireSRWLockExclusive(&w->lock);
  // MEM_RELEASE fails when ANY page in the reservation is still committed.
  // Committed pages can outlive the guest mappings here: recycled host
  // pages (g_allocator batches) were committed by mmap but are tracked
  // only in the interval table, not in guest page tables. Decommit all.
  for (i = 0; i < w->ivn; ++i) {
    VirtualFree((LPVOID)w->iv[i].a, w->iv[i].b - w->iv[i].a, MEM_DECOMMIT);
  }
  w->ivn = 0;
  ReleaseSRWLockExclusive(&w->lock);
  VirtualFree((LPVOID)w->base, 0, MEM_RELEASE);
  free(w->iv);
  free(w);
}

// Release the calling thread's window (must not be window #0) and switch
// back to the primary window. Called when an exec'd vfork child exits.
// Caller must have unmapped all guest pages already (FreeSystem does).
// NB: recycled host pages handed out from this window are purged by the
// caller via WboxPurgeHostPagesInRange() before the reservation goes away.
void WboxMemReleaseWindow(void) {
  struct WboxWindow *w = g_win;
  if (!w || w->index == 0) return;
  g_win = g_windows[0];
  kSkew = g_win ? (uint64_t)g_win->base : 0;
  WboxMemDestroyWindow(w);
}

// ---- snapshot fork support (see WIN32-PORT.md §7.4) ----

// Base/limit of an arbitrary window handle (from WboxMemCurrentWindow).
uintptr_t WboxMemHandleBase(void *win) {
  return win ? ((struct WboxWindow *)win)->base : 0;
}

uintptr_t WboxMemHandleLimit(void *win) {
  return win ? ((struct WboxWindow *)win)->limit : 0;
}

// Full snapshot of the committed pages of `srcwin` into a fresh window.
// Runs on the CALLING thread with explicit window handles (window ops are
// otherwise thread-local): the parent performs the copy synchronously at
// fork() so the parent window can not be mutated or released mid-copy.
// Every committed interval is committed at the same offset in the child,
// its bytes copied, and its host protections replicated region-by-region.
// Returns 0 and stores the new window handle in *dstout, -1 on failure.
int WboxMemSnapshotWindow(void *srcwin, void **dstout) {
  struct WboxWindow *src = (struct WboxWindow *)srcwin;
  struct WboxWindow *dst;
  size_t i, n;
  uintptr_t p;
  int rc = -1;
  if (!src || !dstout) {
    errno = EINVAL;
    return -1;
  }
  if (!(dst = WboxWindowReserve(src->bits))) {
    errno = ENOMEM;
    return -1;
  }
  AcquireSRWLockShared(&src->lock);
  n = src->ivn;
  if (!(dst->iv = malloc((n ? n : 1) * sizeof(*dst->iv)))) {
    ReleaseSRWLockShared(&src->lock);
    goto fail;
  }
  dst->ivcap = n;
  dst->ivn = 0;
  for (i = 0; i < n; ++i) {
    uintptr_t a = src->iv[i].a, b = src->iv[i].b;
    uintptr_t da = dst->base + (a - src->base);
    // the interval table can drift out of sync with reality (a window's
    // committed set changes through munmap/brk/recommit paths): verify
    // the whole range is really committed before copying
    {
      uintptr_t q = a;
      int bad = 0;
      while (q < b) {
        MEMORY_BASIC_INFORMATION mbi;
        if (!VirtualQuery((LPVOID)q, &mbi, sizeof(mbi)) ||
            mbi.State != MEM_COMMIT) {
          if (getenv("WBOX_DEBUG_FORK"))
            fprintf(stderr,
                    "wbox mem: snapshot: stale interval [%p,%p) bad@%p\n",
                    (void *)a, (void *)b, (void *)q);
          bad = 1;
          break;
        }
        q = (uintptr_t)mbi.BaseAddress + mbi.RegionSize;
        if (q <= a || q >= b + 0x1000) q = b;  // defensive
      }
      if (bad) continue;  // skip the stale interval
    }
    if (!VirtualAlloc((LPVOID)da, b - a, MEM_COMMIT, PAGE_READWRITE)) {
      ReleaseSRWLockShared(&src->lock);
      goto fail;
    }
    memcpy((void *)da, (const void *)a, b - a);
    // NB: the child's interval table must hold CHILD addresses — copying
    // src->iv verbatim would make the child's munmap/wipe/release operate
    // on the PARENT's pages (decommitting them under a running parent).
    dst->iv[dst->ivn].a = da;
    dst->iv[dst->ivn].b = dst->base + (b - src->base);
    ++dst->ivn;
    // replicate host protections region-by-region
    p = a;
    while (p < b) {
      MEMORY_BASIC_INFORMATION mbi;
      uintptr_t s, e2;
      if (!VirtualQuery((LPVOID)p, &mbi, sizeof(mbi)) ||
          mbi.State != MEM_COMMIT) {
        ReleaseSRWLockShared(&src->lock);
        goto fail;
      }
      s = (uintptr_t)mbi.BaseAddress;
      e2 = s + mbi.RegionSize;
      if (s < a) s = a;
      if (e2 > b) e2 = b;
      if (mbi.Protect != PAGE_READWRITE) {
        DWORD old;
        if (!VirtualProtect((LPVOID)(dst->base + (s - src->base)), e2 - s,
                            mbi.Protect, &old)) {
          ReleaseSRWLockShared(&src->lock);
          goto fail;
        }
      }
      p = e2;
    }
  }
  dst->hinttop = dst->base + (src->hinttop - src->base);
  ReleaseSRWLockShared(&src->lock);
  *dstout = dst;
  if (getenv("WBOX_DEBUG_MEM"))
    fprintf(stderr, "wbox mem: snapshot window #%d -> #%d (%zu intervals)\n",
            src->index, dst->index, n);
  return 0;
fail:
  AcquireSRWLockExclusive(&g_windows_lock);
  g_windows[dst->index] = 0;
  ReleaseSRWLockExclusive(&g_windows_lock);
  VirtualFree((LPVOID)dst->base, 0, MEM_RELEASE);
  free(dst->iv);
  free(dst);
  errno = ENOMEM;
  return rc;
}

// Decommit every guest page of the calling thread's window and reset the
// allocator bookkeeping, keeping the reservation itself. Used by execve()
// (wipe+reload in place) so a window survives the exec of its System.
// Guest page tables lived in the wiped range; the caller drops cr3 itself
// and must purge recycled host pages via WboxPurgeHostPagesInRange().
void WboxMemWipeWindow(void) {
  struct WboxWindow *w = g_win;
  size_t i;
  if (!w) return;
  AcquireSRWLockExclusive(&w->lock);
  for (i = 0; i < w->ivn; ++i) {
    VirtualFree((LPVOID)w->iv[i].a, w->iv[i].b - w->iv[i].a, MEM_DECOMMIT);
  }
  w->ivn = 0;
  w->hinttop = w->base + 0x10000000;
  ReleaseSRWLockExclusive(&w->lock);
  if (getenv("WBOX_DEBUG_MEM"))
    fprintf(stderr, "wbox mem: wipe window #%d\n", w->index);
}

// Validate a recycled host page (g_allocator freelist) before reuse.
// Such a page can dangle: guest munmap() decommits whole regions without
// consulting the freelist, and an exiting snapshot sibling releases its
// whole window — both leave stale page pointers behind. Returns 1 when
// the page is usable (committed, or recommitted because it belongs to
// one of our live windows), 0 when it must be discarded.
int WboxMemRecommitIfOurs(void *p) {
  MEMORY_BASIC_INFORMATION mbi;
  uintptr_t a = (uintptr_t)p;
  struct WboxWindow *w;
  int rc = 0;
  if (!VirtualQuery((LPVOID)p, &mbi, sizeof(mbi))) return 0;
  // C1 (security-audit): the blink host-page freelist is process-global
  // while each snapshot fork child owns an independent VA window. A
  // recycled page MUST belong to the calling thread's window in every
  // case — a committed page from a sibling window would otherwise be
  // reused as a page-table/data page of THIS guest, letting one guest
  // process read/write another guest's page tables and memory.
  w = g_win;
  if (!w || a < w->base || a >= w->limit) return 0;
  if (mbi.State == MEM_COMMIT) return 1;
  if (mbi.State != MEM_RESERVE) return 0;
  // MEM_RESERVE inside our window: decommitted by a whole-region munmap
  // without consulting the freelist; recommit it.
  AcquireSRWLockExclusive(&w->lock);
  if (VirtualAlloc((LPVOID)a, 4096, MEM_COMMIT, PAGE_READWRITE) &&
      IvInsert(a, a + 4096) == 0) {
    rc = 1;
  } else {
    VirtualFree((LPVOID)a, 4096, MEM_DECOMMIT);
  }
  ReleaseSRWLockExclusive(&w->lock);
  return rc;
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
  // H7: reject lengths that would wrap the page-align or a+len arithmetic
  if (len > (size_t)1 << 48) {
    errno = ENOMEM;
    return MAP_FAILED;
  }
  len = (len + W32PAGE - 1) & ~(W32PAGE - 1);
  if ((uintptr_t)addr & (W32PAGE - 1)) {
    if (getenv("WBOX_DEBUG_MEM"))
      fprintf(stderr, "wbox mmap EINVAL: addr=%p misaligned (W32PAGE=%#x)\n",
              addr, (unsigned)W32PAGE);
    errno = EINVAL;
    return MAP_FAILED;
  }
  AcquireSRWLockExclusive(&g_lock);
  if (flags & MAP_FIXED) {
    a = (uintptr_t)addr;
    if (!W32RangeOk(a, len)) {
      if (getenv("WBOX_DEBUG_MEM"))
        fprintf(stderr,
                "wbox mmap ENOMEM: addr=%p len=%#zx outside [%p,%p)\n", addr,
                len, (void *)g_base, (void *)g_limit);
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
    if (!W32RangeOk(a, len) || IvOverlaps(a, a + len)) {
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
  // H7: no wrap on align, and the range must live inside our window —
  // VirtualFree(MEM_DECOMMIT) on an out-of-window address would hit
  // arbitrary host mappings.
  if (len > (size_t)1 << 48) {
    errno = EINVAL;
    return -1;
  }
  len = (len + W32PAGE - 1) & ~(W32PAGE - 1);
  AcquireSRWLockExclusive(&g_lock);
  if (!W32RangeOk(a, len)) {
    ReleaseSRWLockExclusive(&g_lock);
    errno = EINVAL;
    return -1;
  }
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
  // H7: wrap guard only — blink legitimately mprotects memory OUTSIDE the
  // guest window (the JIT's static g_code[] block pool lives in the exe's
  // BSS), so a window-bounds rejection here is wrong; we only stop
  // pathological lengths.
  if (len > (size_t)1 << 48) {
    errno = EINVAL;
    return -1;
  }
  if (!VirtualProtect((LPVOID)addr, len, W32Prot(prot), &old)) {
    errno = EINVAL;
    return -1;
  }
  return 0;
}

int msync(void *addr, size_t len, int flags) {
  MEMORY_BASIC_INFORMATION mbi;
  // H7: wrap guard only (see mprotect)
  if (len > (size_t)1 << 48) {
    errno = EINVAL;
    return -1;
  }
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
