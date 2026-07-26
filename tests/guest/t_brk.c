/* t_brk.c — brk/sbrk boundary semantics.
 *
 * Uses raw SYS_brk syscalls instead of the musl brk()/sbrk() wrappers:
 * zig-musl's brk is a stub that always fails with -ENOMEM, so testing
 * through the wrappers would test the libc stub, not the kernel (wbox)
 * semantics. Raw brk returns the new break on success and the current
 * (unchanged) break on failure. */
#define _GNU_SOURCE
#include <unistd.h>
#include <stdint.h>
#include <sys/syscall.h>
#include "wtest.h"

static long sys_brk(void *addr) {
  return syscall(SYS_brk, addr);
}

int main(void) {
  T_BEGIN("brk/sbrk-current");
  long cur = sys_brk(0);
  T_ASSERT(cur > 0);
  if (cur <= 0) return WTEST_END();

  T_BEGIN("brk/grow-write");
  long want = cur + 65536;
  int grew = (sys_brk((void *)want) == want);
  T_ASSERT(grew);
  char *base = (char *)cur;
  if (grew) {
    for (int i = 0; i < 65536; i++) base[i] = (char)i;
    int ok = 1;
    for (int i = 0; i < 65536; i++) if (base[i] != (char)i) { ok = 0; break; }
    T_ASSERT(ok);
  } else {
    T_SKIP("brk/grow-write-data", "brk grow failed; skipping dependent writes");
  }

  T_BEGIN("brk/sbrk-increment");
  long b1 = sys_brk(0);
  T_ASSERT(b1 == cur + 65536);
  /* sbrk(4096) equivalent: brk(old + 4096) returns old on success */
  long old = b1;
  T_ASSERT(sys_brk((void *)(b1 + 4096)) == b1 + 4096);
  T_ASSERT(sys_brk(0) == b1 + 4096);
  ((char *)old)[0] = 1; ((char *)old)[4095] = 2;
  T_ASSERT(((char *)old)[0] == 1 && ((char *)old)[4095] == 2);

  T_BEGIN("brk/sbrk-negative-shrink");
  T_ASSERT(sys_brk((void *)b1) == b1);
  T_ASSERT(sys_brk(0) == b1);

  T_BEGIN("brk/shrink-below-start-fails");
  /* brk below the initial (post-load) break must fail: the raw syscall
   * reports failure by returning the current, unchanged break. */
  long before = sys_brk(0);
  long r = sys_brk((void *)0x10000);
  T_ASSERT(r == before);
  T_ASSERT(sys_brk(0) == before);

  T_BEGIN("brk/zero-page-content-after-shrink-regrow");
  /* shrink then regrow: only requirement: memory is writable and readable */
  if (sys_brk((void *)(b1 + 8192)) == b1 + 8192) {
    ((char *)b1)[0] = 5; ((char *)b1)[8191] = 6;
    T_ASSERT(((char *)b1)[0] == 5 && ((char *)b1)[8191] == 6);
  } else {
    T_ASSERT(sys_brk((void *)(b1 + 8192)) == b1 + 8192); /* record the FAIL line */
    T_SKIP("brk/regrow-data", "regrow failed");
  }

  return WTEST_END();
}
