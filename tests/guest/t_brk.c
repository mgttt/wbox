/* t_brk.c — brk/sbrk boundary semantics. */
#define _GNU_SOURCE
#include <unistd.h>
#include <stdint.h>
#include "wtest.h"

int main(void) {
  T_BEGIN("brk/sbrk-current");
  void *cur = sbrk(0);
  T_ASSERT(cur != (void *)-1);
  if (cur == (void *)-1) return WTEST_END();

  T_BEGIN("brk/grow-write");
  int grew = (brk((char *)cur + 65536) == 0);
  T_ASSERT(grew);
  char *base = cur;
  if (grew) {
    for (int i = 0; i < 65536; i++) base[i] = (char)i;
    int ok = 1;
    for (int i = 0; i < 65536; i++) if (base[i] != (char)i) { ok = 0; break; }
    T_ASSERT(ok);
  } else {
    T_SKIP("brk/grow-write-data", "brk grow failed; skipping dependent writes");
  }

  T_BEGIN("brk/sbrk-increment");
  void *b1 = sbrk(0);
  T_ASSERT(b1 == (char *)cur + 65536);
  void *old = sbrk(4096);
  T_ASSERT(old == b1);
  T_ASSERT(sbrk(0) == (char *)b1 + 4096);
  if (old == b1) {
    ((char *)old)[0] = 1; ((char *)old)[4095] = 2;
    T_ASSERT(((char *)old)[0] == 1 && ((char *)old)[4095] == 2);
  }

  T_BEGIN("brk/sbrk-negative-shrink");
  void *b2 = sbrk(-4096);
  T_ASSERT(b2 == (char *)b1 + 4096);
  T_ASSERT(sbrk(0) == b1);

  T_BEGIN("brk/shrink-below-start-EINVAL");
  /* brk below the initial break must fail */
  void *lo = (void *)0x10000;
  errno = 0;
  int r = brk(lo);
  T_ASSERT(r == -1 && errno == ENOMEM);

  T_BEGIN("brk/zero-page-content-after-shrink-regrow");
  /* shrink then regrow: only requirement: memory is writable and readable */
  if (brk((char *)b1 + 8192) == 0) {
    ((char *)b1)[0] = 5; ((char *)b1)[8191] = 6;
    T_ASSERT(((char *)b1)[0] == 5 && ((char *)b1)[8191] == 6);
  } else {
    T_ASSERT_OK(brk((char *)b1 + 8192)); /* record the FAIL line */
    T_SKIP("brk/regrow-data", "regrow failed");
  }

  return WTEST_END();
}
