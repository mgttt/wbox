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

intptr_t _get_osfhandle(int);

// MAP_SHARED file mappings: dup'd fd + file offset per committed range,
// written back on msync()/munmap(). The shim populates shared mappings by
// pread() just like private ones, so without this registry guest writes
// to a MAP_SHARED file mapping never reached the host file.
struct Fshare {
  uintptr_t lo, hi;  // committed range (page aligned)
  int fd;            // dup() of the mapping fd; closed when fully unmapped
  int used;
  off_t off;         // file offset corresponding to lo
  DWORD volume;
  DWORD file_hi;
  DWORD file_lo;
  int identified;
};

struct Fshares {
  struct Fshare *p;
  size_t n;
};

// SysMunmap preflights writeback and any metadata split before guest page
// tables change. These fields carry that transaction into the host munmap.
static _Thread_local uintptr_t g_prepared_unmap_lo;
static _Thread_local uintptr_t g_prepared_unmap_hi;
static _Thread_local size_t g_prepared_split_src;
static _Thread_local size_t g_prepared_split_dst;
static _Thread_local int g_prepared_split_fd;
static _Thread_local int g_prepared_split;
static _Thread_local int g_prepared_unmap;

static int FshareFindFree(struct Fshares *table, size_t *slot) {
  struct Fshare *p;
  size_t i, n;
  for (i = 0; i < table->n; ++i) {
    if (!table->p[i].used) {
      *slot = i;
      return 0;
    }
  }
  if (table->n > SIZE_MAX / 2 / sizeof(*p)) {
    errno = ENOMEM;
    return -1;
  }
  n = table->n ? table->n * 2 : 16;
  if (!(p = realloc(table->p, n * sizeof(*p)))) return -1;
  memset(p + table->n, 0, (n - table->n) * sizeof(*p));
  table->p = p;
  i = table->n;
  table->n = n;
  *slot = i;
  return 0;
}

static void FshareIdentify(struct Fshare *entry) {
  BY_HANDLE_FILE_INFORMATION info;
  intptr_t raw = _get_osfhandle(entry->fd);
  if (raw != -1 &&
      GetFileInformationByHandle((HANDLE)raw, &info)) {
    entry->volume = info.dwVolumeSerialNumber;
    entry->file_hi = info.nFileIndexHigh;
    entry->file_lo = info.nFileIndexLow;
    entry->identified = 1;
  }
}

static int FshareRegister(struct Fshares *table, uintptr_t lo, uintptr_t hi,
                          int fd, off_t off) {
  int d;
  size_t i;
  if (FshareFindFree(table, &i) == -1) return -1;
  if ((d = dup(fd)) == -1) return -1;
  memset(table->p + i, 0, sizeof(table->p[i]));
  table->p[i].lo = lo;
  table->p[i].hi = hi;
  table->p[i].fd = d;
  table->p[i].off = off;
  FshareIdentify(table->p + i);
  table->p[i].used = 1;
  return 0;
}

static int FshareShouldFailWriteback(void) {
  static LONG calls;
  static LONG once;
  const char *stage = getenv("WBOX_TEST_FSHARE_FAIL");
  if (!stage) return 0;
  if (!strcmp(stage, "writeback")) {
    return InterlockedCompareExchange(&once, 1, 0) == 0;
  }
  if (!strcmp(stage, "writeback-second")) {
    return InterlockedIncrement(&calls) == 2;
  }
  return 0;
}

static int FshareShouldFailSplit(void) {
  static LONG once;
  const char *stage = getenv("WBOX_TEST_FSHARE_FAIL");
  return stage && !strcmp(stage, "split") &&
         InterlockedCompareExchange(&once, 1, 0) == 0;
}

// write back the overlap of [a,b) with every registered shared mapping
static int FshareWriteback(struct Fshares *table, uintptr_t a, uintptr_t b) {
  int result = 0;
  int saved_errno = 0;
  size_t i;
  for (i = 0; i < table->n; ++i) {
    uintptr_t lo, hi, p;
    if (!table->p[i].used) continue;
    lo = a > table->p[i].lo ? a : table->p[i].lo;
    hi = b < table->p[i].hi ? b : table->p[i].hi;
    if (lo >= hi) continue;
    if (FshareShouldFailWriteback()) {
      result = -1;
      if (!saved_errno) saved_errno = EIO;
      continue;
    }
    for (p = lo; p < hi;) {
      MEMORY_BASIC_INFORMATION mbi;
      uintptr_t end;
      DWORD old = 0;
      int changed = 0;
      size_t done = 0;
      if (!VirtualQuery((void *)p, &mbi, sizeof(mbi)) ||
          mbi.State != MEM_COMMIT) {
        result = -1;
        if (!saved_errno) saved_errno = EIO;
        break;
      }
      end = (uintptr_t)mbi.BaseAddress + mbi.RegionSize;
      if (end > hi) end = hi;
      if ((mbi.Protect & PAGE_GUARD) ||
          !(mbi.Protect &
            (PAGE_READONLY | PAGE_READWRITE | PAGE_WRITECOPY |
             PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE |
             PAGE_EXECUTE_WRITECOPY))) {
        if (!VirtualProtect((void *)p, end - p, PAGE_READONLY, &old)) {
          result = -1;
          if (!saved_errno) saved_errno = EIO;
          break;
        }
        changed = 1;
      }
      while (done < (size_t)(end - p)) {
        ssize_t rc =
            pwrite(table->p[i].fd, (void *)(p + done),
                   (size_t)(end - p) - done,
                   table->p[i].off + (off_t)(p - table->p[i].lo) +
                       (off_t)done);
        if (rc <= 0) {
          result = -1;
          if (!saved_errno) {
            saved_errno = rc < 0 && errno ? errno : EIO;
          }
          break;
        }
        done += (size_t)rc;
      }
      if (changed) {
        DWORD ignored;
        if (!VirtualProtect((void *)p, end - p, old, &ignored)) {
          result = -1;
          if (!saved_errno) saved_errno = EIO;
        }
      }
      if (done != (size_t)(end - p)) break;
      p = end;
    }
  }
  if (result == -1) errno = saved_errno;
  return result;
}

// drop [a,b) from the registry (munmap), trimming/splitting entries
static int FshareRemove(struct Fshares *table, uintptr_t a, uintptr_t b) {
  size_t i;
  for (i = 0; i < table->n; ++i) {
    uintptr_t lo, hi;
    if (!table->p[i].used) continue;
    if (table->p[i].lo >= b || table->p[i].hi <= a) continue;
    lo = table->p[i].lo;
    hi = table->p[i].hi;
    if (a <= lo && b >= hi) {
      close(table->p[i].fd);
      table->p[i].used = 0;
    } else if (a <= lo) {
      table->p[i].off += (off_t)(b - lo);
      table->p[i].lo = b;
    } else if (b >= hi) {
      table->p[i].hi = a;
    } else {
      // punch a hole: keep the tail in this slot, move the head to a
      // free slot with its own dup() so either can be closed alone
      int d;
      size_t j;
      if (g_prepared_split && i == g_prepared_split_src &&
          a >= g_prepared_unmap_lo && b <= g_prepared_unmap_hi) {
        j = g_prepared_split_dst;
        d = g_prepared_split_fd;
        g_prepared_split_fd = -1;
        g_prepared_split = 0;
      } else if (FshareFindFree(table, &j) == -1 ||
                 (d = dup(table->p[i].fd)) == -1) {
        return -1;
      }
      table->p[j] = table->p[i];
      table->p[j].lo = lo;
      table->p[j].hi = a;
      table->p[j].fd = d;
      table->p[j].off = table->p[i].off;
      table->p[i].off += (off_t)(b - lo);
      table->p[i].lo = b;
    }
  }
  return 0;
}

static void FshareClear(struct Fshares *table, int writeback) {
  size_t i;
  if (writeback) (void)FshareWriteback(table, 0, UINTPTR_MAX);
  for (i = 0; i < table->n; ++i) {
    if (table->p[i].used) close(table->p[i].fd);
  }
  if (table->n) memset(table->p, 0, table->n * sizeof(*table->p));
}

static int FshareSameFile(const struct Fshare *a,
                          const struct Fshare *b) {
  return a->identified && b->identified &&
         a->volume == b->volume &&
         a->file_hi == b->file_hi &&
         a->file_lo == b->file_lo;
}

static void FshareSync(struct Fshares *src, struct Fshares *dst,
                       uintptr_t rangelo, uintptr_t rangehi) {
  size_t i, j;
  for (i = 0; i < src->n; ++i) {
    uintptr_t srclo, srchi;
    uint64_t srcoff, srcend;
    if (!src->p[i].used) continue;
    srclo = src->p[i].lo > rangelo ? src->p[i].lo : rangelo;
    srchi = src->p[i].hi < rangehi ? src->p[i].hi : rangehi;
    if (srclo >= srchi) continue;
    if (src->p[i].off < 0) continue;
    srcoff = (uint64_t)src->p[i].off;
    if (srclo - src->p[i].lo > UINT64_MAX - srcoff) continue;
    srcoff += srclo - src->p[i].lo;
    if (srchi - srclo > UINT64_MAX - srcoff) continue;
    srcend = srcoff + (srchi - srclo);
    for (j = 0; j < dst->n; ++j) {
      uint64_t dstoff, dstend, off, end;
      uintptr_t sa, da;
      if (!dst->p[j].used ||
          !FshareSameFile(src->p + i, dst->p + j)) continue;
      if (dst->p[j].off < 0) continue;
      dstoff = (uint64_t)dst->p[j].off;
      if (dst->p[j].hi - dst->p[j].lo > UINT64_MAX - dstoff) continue;
      dstend = dstoff + (dst->p[j].hi - dst->p[j].lo);
      off = srcoff > dstoff ? srcoff : dstoff;
      end = srcend < dstend ? srcend : dstend;
      if (off >= end) continue;
      sa = srclo + (uintptr_t)(off - srcoff);
      da = dst->p[j].lo + (uintptr_t)(off - dstoff);
      while (off < end) {
        MEMORY_BASIC_INFORMATION smbi, dmbi;
        size_t amount =
            (size_t)(end - off > W32PAGE ? W32PAGE : end - off);
        DWORD sold = 0, dold = 0, ignored;
        int schanged = 0, dchanged = 0;
        if (!VirtualQuery((void *)sa, &smbi, sizeof(smbi)) ||
            smbi.State != MEM_COMMIT ||
            !VirtualQuery((void *)da, &dmbi, sizeof(dmbi)) ||
            dmbi.State != MEM_COMMIT) {
          goto next_page;
        }
        if ((smbi.Protect & PAGE_GUARD) ||
            !(smbi.Protect &
              (PAGE_READONLY | PAGE_READWRITE | PAGE_WRITECOPY |
               PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE |
               PAGE_EXECUTE_WRITECOPY))) {
          if (!VirtualProtect((void *)sa, amount, PAGE_READONLY, &sold)) {
            goto next_page;
          }
          schanged = 1;
        }
        if ((dmbi.Protect & PAGE_GUARD) ||
            !(dmbi.Protect &
              (PAGE_READWRITE | PAGE_WRITECOPY |
               PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY))) {
          if (!VirtualProtect((void *)da, amount, PAGE_READWRITE, &dold)) {
            if (schanged)
              VirtualProtect((void *)sa, amount, sold, &ignored);
            goto next_page;
          }
          dchanged = 1;
        }
        memcpy((void *)da, (void *)sa, amount);
        if (dchanged) VirtualProtect((void *)da, amount, dold, &ignored);
        if (schanged) VirtualProtect((void *)sa, amount, sold, &ignored);
next_page:
        sa += amount;
        da += amount;
        off += amount;
      }
    }
  }
}

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
  struct Fshares fshare;
  struct WboxWindow *sparent;  // snapshot parent window (weak, see sync)
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

static void SharedSync(struct WboxWindow *w);
static void SharedSyncRangeLocked(struct WboxWindow *w, uintptr_t lo,
                                  uintptr_t hi);
void WboxMemSyncSharedToParent(void) {
  SharedSync(g_win);
}

int WboxMemPrepareUnmap(uintptr_t a, size_t len) {
  int rc;
  size_t i;
  struct WboxWindow *w = g_win;
  g_prepared_unmap = 0;
  if (g_prepared_split) {
    close(g_prepared_split_fd);
    g_prepared_split = 0;
  }
  if (!w || len > UINTPTR_MAX - a) {
    errno = EINVAL;
    return -1;
  }
  AcquireSRWLockExclusive(&w->lock);
  rc = FshareWriteback(&w->fshare, a, a + len);
  if (rc == 0) {
    for (i = 0; i < w->fshare.n; ++i) {
      int d;
      size_t j;
      if (!w->fshare.p[i].used ||
          a <= w->fshare.p[i].lo ||
          a + len >= w->fshare.p[i].hi) {
        continue;
      }
      if (FshareShouldFailSplit()) {
        errno = EMFILE;
        rc = -1;
        break;
      }
      if (FshareFindFree(&w->fshare, &j) == -1 ||
          (d = dup(w->fshare.p[i].fd)) == -1) {
        rc = -1;
        break;
      }
      g_prepared_split_src = i;
      g_prepared_split_dst = j;
      g_prepared_split_fd = d;
      g_prepared_split = 1;
      break;
    }
  }
  ReleaseSRWLockExclusive(&w->lock);
  if (rc == 0) {
    g_prepared_unmap_lo = a;
    g_prepared_unmap_hi = a + len;
    g_prepared_unmap = 1;
  }
  return rc;
}

void WboxMemFinishUnmap(void) {
  g_prepared_unmap = 0;
  if (g_prepared_split) {
    close(g_prepared_split_fd);
    g_prepared_split = 0;
  }
}

// Caller holds w->lock. Pin and lock the snapshot parent, then publish only
// the range that is about to lose its child Fshare metadata.
static void SharedSyncRangeLocked(struct WboxWindow *w, uintptr_t lo,
                                  uintptr_t hi) {
  struct WboxWindow *p;
  size_t i;
  if (!w || !(p = w->sparent) || lo >= hi) return;
  AcquireSRWLockShared(&g_windows_lock);
  for (i = 0; i < WBOX_MAX_WINDOWS && g_windows[i] != p; ++i)
    ;
  if (i == WBOX_MAX_WINDOWS) {
    ReleaseSRWLockShared(&g_windows_lock);
    w->sparent = 0;
    return;
  }
  AcquireSRWLockShared(&p->lock);
  ReleaseSRWLockShared(&g_windows_lock);
  FshareSync(&w->fshare, &p->fshare, lo, hi);
  ReleaseSRWLockShared(&p->lock);
}

// Copy shared file mappings of window `w` back into its snapshot parent.
// MAP_SHARED|MAP_ANONYMOUS uses an unlinked temporary file on Windows, so
// it follows this same file-ID/offset path without separate metadata.
static void SharedSync(struct WboxWindow *w) {
  struct WboxWindow *p;
  size_t i;
  if (!w || !(p = w->sparent)) return;
  // liveness: sparent dangles once the parent window is destroyed. The
  // destroy path detaches from g_windows under g_windows_lock exclusive
  // before freeing, so verify membership while holding it shared, and pin
  // the parent with its own lock (destroy takes it exclusive) across the
  // copy. The g_windows_lock is held until p->lock is acquired so the
  // parent cannot disappear in between.
  AcquireSRWLockShared(&g_windows_lock);
  for (i = 0; i < WBOX_MAX_WINDOWS && g_windows[i] != p; ++i)
    ;
  if (i == WBOX_MAX_WINDOWS) {
    ReleaseSRWLockShared(&g_windows_lock);
    w->sparent = 0;
    return;
  }
  AcquireSRWLockShared(&p->lock);
  ReleaseSRWLockShared(&g_windows_lock);
  AcquireSRWLockShared(&w->lock);
  (void)FshareWriteback(&w->fshare, 0, UINTPTR_MAX);
  FshareSync(&w->fshare, &p->fshare, 0, UINTPTR_MAX);
  ReleaseSRWLockShared(&w->lock);
  ReleaseSRWLockShared(&p->lock);
}

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
    fflush(stderr);
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
  // wbox: install the VEH + console ctrl handler. This was previously
  // never called anywhere, so a guest page fault went straight to the
  // wine default handler and killed the whole process.
  WboxSigInit();
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
  // Flush this window's shared mappings to its snapshot parent
  // before the pages go away (failure paths; the normal exit path synced
  // earlier via WboxMemSyncSharedToParent while guest pages were still live)
  SharedSync(w);
  // H5 (security-audit): detach from the global table first, then take
  // the window lock exclusively before touching the interval table and
  // freeing — a concurrent WboxMemSnapshotWindow holds src->lock shared
  // while reading the same table, and WboxMemRecommitIfOurs takes it
  // exclusive. Releasing the reservation without the lock raced both.
  AcquireSRWLockExclusive(&g_windows_lock);
  g_windows[w->index] = 0;
  // any window naming this one as its snapshot parent now has a dangling
  // sparent: clear it while the table lock makes the pointer check race-
  // free against SharedSync's membership scan
  for (i = 0; i < WBOX_MAX_WINDOWS; ++i) {
    if (g_windows[i] && g_windows[i]->sparent == w) {
      g_windows[i]->sparent = 0;
    }
  }
  ReleaseSRWLockExclusive(&g_windows_lock);
  AcquireSRWLockExclusive(&w->lock);
  FshareClear(&w->fshare, 1);
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
  free(w->fshare.p);
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
  if (getenv("WBOX_DEBUG_FORK"))
    fprintf(stderr, "wbox mem: snapshot: acquiring src lock\n");
    fflush(stderr);
  AcquireSRWLockShared(&src->lock);
  if (getenv("WBOX_DEBUG_FORK"))
    fprintf(stderr, "wbox mem: snapshot: src lock held, %zu intervals\n",
            (size_t)src->ivn);
    fflush(stderr);
  // Shared mappings keep their backing descriptors and are synced back into
  // this window by file identity/offset when the child exits.
  dst->sparent = src;
  if (src->fshare.n &&
      !(dst->fshare.p = calloc(src->fshare.n,
                               sizeof(*dst->fshare.p)))) {
    ReleaseSRWLockShared(&src->lock);
    goto fail;
  }
  dst->fshare.n = src->fshare.n;
  for (i = 0; i < src->fshare.n; ++i) {
    int d;
    if (!src->fshare.p[i].used) continue;
    if ((d = dup(src->fshare.p[i].fd)) == -1) {
      ReleaseSRWLockShared(&src->lock);
      goto fail;
    }
    dst->fshare.p[i] = src->fshare.p[i];
    dst->fshare.p[i].lo =
        dst->base + (src->fshare.p[i].lo - src->base);
    dst->fshare.p[i].hi =
        dst->base + (src->fshare.p[i].hi - src->base);
    dst->fshare.p[i].fd = d;
  }
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
    fflush(stderr);
          bad = 1;
          break;
        }
        q = (uintptr_t)mbi.BaseAddress + mbi.RegionSize;
        if (q <= a || q >= b + 0x1000) q = b;  // defensive
      }
      if (bad) continue;  // skip the stale interval
    }
    // 逐区间打点：真机 fork 挂死（KNOWN-FAILURES W1）已定位到本函数内部，
    // 需要区分"卡在某个巨大区间的 commit/memcpy"与"卡在锁上"。
    if (getenv("WBOX_DEBUG_FORK"))
      fprintf(stderr, "wbox mem: snapshot: iv[%zu] [%p,%p) %zu bytes\n",
              (size_t)i, (void *)a, (void *)b, (size_t)(b - a));
    fflush(stderr);
    if (!VirtualAlloc((LPVOID)da, b - a, MEM_COMMIT, PAGE_READWRITE)) {
      ReleaseSRWLockShared(&src->lock);
      goto fail;
    }

    // NB: the child's interval table must hold CHILD addresses — copying
    // src->iv verbatim would make the child's munmap/wipe/release operate
    // on the PARENT's pages (decommitting them under a running parent).
    dst->iv[dst->ivn].a = da;
    dst->iv[dst->ivn].b = dst->base + (b - src->base);
    ++dst->ivn;
    // 逐区域「拷贝 + 复制保护属性」。
    //
    // 这里**必须**按区域走，不能对整个区间做一次 memcpy：区间只被验证过
    // MEM_COMMIT（提交状态），而**已提交 ≠ 可读**——guest 的 PROT_NONE 映射
    // 与 guard 页同样是 committed。整段 memcpy 一旦读到 PAGE_NOACCESS/
    // PAGE_GUARD 页就触发访问违例，而 fork 期间 m->canhalt 为假、VEH 不按
    // guest 缺页处理，故障指令被反复重试 → 表现为**永久挂死**（真 Windows
    // 与 wine 9.0 均可复现，见 tests/KNOWN-FAILURES.md W1；旧顺序是"先整段
    // 拷贝、后逐区域套保护"，恰好把这个雷埋在拷贝里）。
    //
    // 不可读区域无需拷贝：其内容对 guest 本就不可访问，子进程只要拿到同样的
    // 保护属性即可，语义完全一致。
    p = a;
    while (p < b) {
      MEMORY_BASIC_INFORMATION mbi;
      uintptr_t s, e2;
      int readable;
      if (!VirtualQuery((LPVOID)p, &mbi, sizeof(mbi)) ||
          mbi.State != MEM_COMMIT) {
        ReleaseSRWLockShared(&src->lock);
        goto fail;
      }
      s = (uintptr_t)mbi.BaseAddress;
      e2 = s + mbi.RegionSize;
      if (s < a) s = a;
      if (e2 > b) e2 = b;
      readable = !(mbi.Protect & (PAGE_NOACCESS | PAGE_GUARD));
      if (readable) {
        memcpy((void *)(dst->base + (s - src->base)), (const void *)s, e2 - s);
      } else if (getenv("WBOX_DEBUG_FORK")) {
        fprintf(stderr,
                "wbox mem: snapshot: skip unreadable [%p,%p) prot=%#lx\n",
                (void *)s, (void *)e2, (unsigned long)mbi.Protect);
        fflush(stderr);
      }
      // 保护属性在拷贝之后再套：dst 整段以 PAGE_READWRITE 提交，先拷后改，
      // 否则给 dst 套上 PAGE_NOACCESS 会让后续区域的写入自己也炸。
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
    fflush(stderr);
  return 0;
fail:
  AcquireSRWLockExclusive(&g_windows_lock);
  g_windows[dst->index] = 0;
  ReleaseSRWLockExclusive(&g_windows_lock);
  FshareClear(&dst->fshare, 0);
  VirtualFree((LPVOID)dst->base, 0, MEM_RELEASE);
  free(dst->fshare.p);
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
  FshareClear(&w->fshare, 1);
  for (i = 0; i < w->ivn; ++i) {
    VirtualFree((LPVOID)w->iv[i].a, w->iv[i].b - w->iv[i].a, MEM_DECOMMIT);
  }
  w->ivn = 0;
  w->sparent = 0;  // nothing to sync back at this window's later exit
  w->hinttop = w->base + 0x10000000;
  ReleaseSRWLockExclusive(&w->lock);
  if (getenv("WBOX_DEBUG_MEM"))
    fprintf(stderr, "wbox mem: wipe window #%d\n", w->index);
    fflush(stderr);
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
      if (FshareWriteback(&g_win->fshare, a, a + len) == -1) {
        ReleaseSRWLockExclusive(&g_lock);
        return MAP_FAILED;
      }
      SharedSyncRangeLocked(g_win, a, a + len);
      if (FshareRemove(&g_win->fshare, a, a + len) == -1) {
        ReleaseSRWLockExclusive(&g_lock);
        return MAP_FAILED;
      }
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
  if ((flags & MAP_SHARED) && !(flags & MAP_ANONYMOUS) && fd >= 0) {
    if (FshareRegister(&g_win->fshare, a, a + len, fd, off) == -1) {
      int err = errno;
      IvRemove(a, a + len);
      VirtualFree((LPVOID)a, len, MEM_DECOMMIT);
      ReleaseSRWLockExclusive(&g_lock);
      errno = err;
      return MAP_FAILED;
    }
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
  }
  return r;
}

int munmap(void *addr, size_t len) {
  uintptr_t a = (uintptr_t)addr;
  int prepared;
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
  prepared = g_prepared_unmap &&
             a >= g_prepared_unmap_lo &&
             a + len <= g_prepared_unmap_hi;
  if (!prepared &&
      FshareWriteback(&g_win->fshare, a, a + len) == -1) {
    ReleaseSRWLockExclusive(&g_lock);
    return -1;
  }
  SharedSyncRangeLocked(g_win, a, a + len);
  if (FshareRemove(&g_win->fshare, a, a + len) == -1) {
    ReleaseSRWLockExclusive(&g_lock);
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
  int rc;
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
  AcquireSRWLockExclusive(&g_lock);
  rc = FshareWriteback(&g_win->fshare, (uintptr_t)addr,
                       (uintptr_t)addr + len);
  if (rc == 0) {
    SharedSyncRangeLocked(g_win, (uintptr_t)addr,
                          (uintptr_t)addr + len);
  }
  ReleaseSRWLockExclusive(&g_lock);
  return rc;
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
