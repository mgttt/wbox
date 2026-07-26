/* t_mmap.c — mmap/munmap/mprotect semantics regression suite. */
#define _GNU_SOURCE
#include <sys/mman.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
#include <stdint.h>
#include "wtest.h"

#define PGSZ 4096

int main(void) {
  /* --- anonymous private --- */
  T_BEGIN("mmap/anon-private-zeroed");
  char *p = mmap(NULL, PGSZ * 4, PROT_READ | PROT_WRITE,
                 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  T_ASSERT(p != MAP_FAILED);
  T_ASSERT_OK(munmap(p, PGSZ * 4));

  T_BEGIN("mmap/anon-private-zero-content");
  p = mmap(NULL, PGSZ, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  T_ASSERT(p != MAP_FAILED);
  if (p == MAP_FAILED) return WTEST_END();
  int zero = 1;
  for (int i = 0; i < PGSZ; i++) if (p[i]) { zero = 0; break; }
  T_ASSERT(zero);

  T_BEGIN("mmap/write-readback");
  for (int i = 0; i < PGSZ; i++) p[i] = (char)(i & 0xff);
  int ok = 1;
  for (int i = 0; i < PGSZ; i++) if (p[i] != (char)(i & 0xff)) { ok = 0; break; }
  T_ASSERT(ok);

  /* --- MAP_FIXED --- */
  T_BEGIN("mmap/fixed-replace");
  char *q = mmap(p, PGSZ, PROT_READ | PROT_WRITE,
                 MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
  T_ASSERT(q == p);
  T_ASSERT(q != MAP_FAILED && q[0] == 0); /* fresh zero page replaced old */

  T_BEGIN("mmap/fixed-unaligned-EINVAL");
  T_ASSERT_ERRNO(mmap(p + 1, PGSZ, PROT_READ,
                      MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0), EINVAL);

  T_BEGIN("mmap/bad-prot-none-then-rw");
  void *n = mmap(NULL, PGSZ, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  T_ASSERT(n != MAP_FAILED);

  /* --- mprotect transitions --- */
  T_BEGIN("mprotect/none-to-rw");
  T_ASSERT_OK(mprotect(n, PGSZ, PROT_READ | PROT_WRITE));
  if (n != MAP_FAILED) {
    ((char *)n)[123] = 42;
    T_ASSERT(((char *)n)[123] == 42);
  }

  T_BEGIN("mprotect/rw-to-r-readback");
  T_ASSERT_OK(mprotect(n, PGSZ, PROT_READ));
  if (n != MAP_FAILED) T_ASSERT(((char *)n)[123] == 42);
  T_ASSERT_OK(munmap(n, PGSZ));

  /* --- partial unmap --- */
  T_BEGIN("munmap/partial");
  char *m = mmap(NULL, PGSZ * 4, PROT_READ | PROT_WRITE,
                 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  T_ASSERT(m != MAP_FAILED);
  if (m != MAP_FAILED) {
    for (int i = 0; i < PGSZ * 4; i++) m[i] = 1;
    T_ASSERT_OK(munmap(m + PGSZ, PGSZ * 2)); /* drop middle two pages */
    m[0] = 7;                 /* first page still live */
    m[PGSZ * 3] = 9;          /* last page still live */
    T_ASSERT(m[0] == 7 && m[PGSZ * 3] == 9);
    /* remap the hole with MAP_FIXED and verify independence */
    char *h = mmap(m + PGSZ, PGSZ * 2, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    T_ASSERT(h == m + PGSZ);
    T_ASSERT(m[0] == 7 && m[PGSZ * 3] == 9);
    T_ASSERT_OK(munmap(m, PGSZ * 4));
  }

  T_BEGIN("munmap/invalid-EINVAL");
  T_ASSERT_ERRNO(munmap((void *)0x1000, PGSZ), EINVAL);

  /* --- huge (2MiB) aligned anonymous map --- */
  T_BEGIN("mmap/huge-2m-aligned");
  size_t big = 2 * 1024 * 1024;
  char *hb = mmap(NULL, big * 2, PROT_READ | PROT_WRITE,
                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  T_ASSERT(hb != MAP_FAILED);
  if (hb != MAP_FAILED) {
    /* align up to 2MiB and MAP_FIXED a fresh map there */
    uintptr_t al = ((uintptr_t)hb + big - 1) & ~(uintptr_t)(big - 1);
    char *f = mmap((void *)al, big, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    T_ASSERT(f == (void *)al);
    if (f != MAP_FAILED) {
      f[0] = 1; f[big - 1] = 2;
      T_ASSERT(f[0] == 1 && f[big - 1] == 2);
    }
    T_ASSERT_OK(munmap(hb, big * 2));
  }

  /* --- file-backed mapping --- */
  T_BEGIN("mmap/file-private-readback");
  int fd = open("t_mmap_tmp.bin", O_RDWR | O_CREAT | O_TRUNC, 0600);
  T_ASSERT(fd >= 0);
  if (fd >= 0) {
    char buf[PGSZ];
    for (int i = 0; i < PGSZ; i++) buf[i] = (char)(i * 7 + 1);
    T_ASSERT_EQ(write(fd, buf, PGSZ), PGSZ);
    char *fm = mmap(NULL, PGSZ, PROT_READ, MAP_PRIVATE, fd, 0);
    T_ASSERT(fm != MAP_FAILED);
    if (fm != MAP_FAILED) {
      T_ASSERT_EQ(memcmp(fm, buf, PGSZ), 0);
      T_ASSERT_OK(munmap(fm, PGSZ));
    }
    /* shared writable file mapping persists to disk */
    T_BEGIN("mmap/file-shared-persist");
    fm = mmap(NULL, PGSZ, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    T_ASSERT(fm != MAP_FAILED);
    if (fm != MAP_FAILED) {
      fm[0] = (char)0xAA;
      T_ASSERT_OK(msync(fm, PGSZ, MS_SYNC));
      T_ASSERT_OK(munmap(fm, PGSZ));
      char rb;
      T_ASSERT_EQ(pread(fd, &rb, 1, 0), 1);
      T_ASSERT(rb == (char)0xAA);
    }
    close(fd);
    unlink("t_mmap_tmp.bin");
  }

  /* --- invalid combos must fail --- */
  T_BEGIN("mmap/file-badfd-EBADF");
  T_ASSERT_ERRNO(mmap(NULL, PGSZ, PROT_READ, MAP_PRIVATE, 9999, 0), EBADF);

  T_BEGIN("mmap/len-zero-EINVAL");
  T_ASSERT_ERRNO(mmap(NULL, 0, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0), EINVAL);

  T_BEGIN("mmap/anon-with-fd-flag-mix");
  /* MAP_ANONYMOUS requires fd == -1 on Linux */
  {
    int d = open("/dev/null", O_RDONLY);
    errno = 0;
    void *r = mmap(NULL, PGSZ, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, d, 0);
    if (r == MAP_FAILED && errno == EBADF) wtest_pass++, printf("PASS %s\n", wtest_cur);
    else { wtest_fail++; printf("FAIL %s: anon+fd=%d => %p errno=%d\n",
                                wtest_cur, d, r, errno); }
    close(d);
  }

  return WTEST_END();
}
