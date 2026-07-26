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

static int writeback_failure_probe(const char *operation) {
  size_t maplen = !strcmp(operation, "split") ? PGSZ * 3 : PGSZ;
  unsigned char value = 0;
  int fd = open("t_mmap_writeback_failure.bin",
                O_RDWR | O_CREAT | O_TRUNC, 0600);
  if (fd == -1 || ftruncate(fd, maplen) == -1) return 1;
  unsigned char *map =
      mmap(NULL, maplen, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
  if (map == MAP_FAILED) return 2;
  map[0] = 0xa7;
  errno = 0;
  if (!strcmp(operation, "msync")) {
    if (msync(map, PGSZ, MS_SYNC) != -1 || errno != EIO) return 3;
    if (map[0] != 0xa7) return 4;
    if (msync(map, PGSZ, MS_SYNC) == -1) return 5;
    if (munmap(map, PGSZ) == -1) return 6;
  } else if (!strcmp(operation, "munmap")) {
    if (munmap(map, PGSZ) != -1 || errno != EIO) return 9;
    if (map[0] != 0xa7) return 10;
    if (munmap(map, PGSZ) == -1) return 11;
  } else if (!strcmp(operation, "munmap-second")) {
    if (munmap(map, PGSZ) == -1) return 17;
  } else if (!strcmp(operation, "split")) {
    unsigned char values[3] = {0};
    map[0] = 0xa1;
    map[PGSZ] = 0xa2;
    map[PGSZ * 2] = 0xa3;
    if (munmap(map + PGSZ, PGSZ) != -1 || errno != EMFILE) return 18;
    if (map[0] != 0xa1 || map[PGSZ] != 0xa2 ||
        map[PGSZ * 2] != 0xa3) {
      return 19;
    }
    if (munmap(map + PGSZ, PGSZ) == -1) return 20;
    if (munmap(map, PGSZ) == -1 ||
        munmap(map + PGSZ * 2, PGSZ) == -1) {
      return 21;
    }
    for (int i = 0; i < 3; ++i) {
      if (pread(fd, values + i, 1, (off_t)i * PGSZ) != 1) return 22;
    }
    if (values[0] != 0xa1 || values[1] != 0xa2 || values[2] != 0xa3) {
      return 23;
    }
    if (close(fd) == -1 ||
        unlink("t_mmap_writeback_failure.bin") == -1) {
      return 24;
    }
    return 0;
  } else if (!strcmp(operation, "fixed")) {
    void *replacement =
        mmap(map, PGSZ, PROT_READ | PROT_WRITE,
             MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    if (replacement != MAP_FAILED || errno != EIO) return 12;
    if (map[0] != 0xa7) return 13;
    replacement =
        mmap(map, PGSZ, PROT_READ | PROT_WRITE,
             MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    if (replacement != map || map[0] != 0) return 14;
    if (munmap(map, PGSZ) == -1) return 15;
  } else {
    return 16;
  }
  if (pread(fd, &value, 1, 0) != 1 || value != 0xa7) return 7;
  if (close(fd) == -1 || unlink("t_mmap_writeback_failure.bin") == -1) {
    return 8;
  }
  return 0;
}

int main(int argc, char **argv) {
  if (argc == 2 &&
      !strncmp(argv[1], "--writeback-failure-", 20)) {
    return writeback_failure_probe(argv[1] + 20);
  }

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

  T_BEGIN("munmap/unmapped-succeeds");
  /* real Linux: munmap of an unmapped (but valid) range succeeds */
  T_ASSERT_OK(munmap((void *)0x1000, PGSZ));

  /* --- mremap anonymous mappings --- */
  T_BEGIN("mremap/shrink-in-place");
  char *mr = mmap(NULL, PGSZ * 3, PROT_READ | PROT_WRITE,
                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  T_ASSERT(mr != MAP_FAILED);
  if (mr != MAP_FAILED) {
    mr[0] = 17;
    mr[PGSZ - 1] = 23;
    char *shrunk = mremap(mr, PGSZ * 3, PGSZ, 0);
    T_ASSERT(shrunk == mr);
    if (shrunk != MAP_FAILED) {
      T_ASSERT(shrunk[0] == 17 && shrunk[PGSZ - 1] == 23);
      T_ASSERT_OK(munmap(shrunk, PGSZ));
    } else {
      T_ASSERT_OK(munmap(mr, PGSZ * 3));
    }
  }

  T_BEGIN("mremap/grow-in-place");
  mr = mmap(NULL, PGSZ * 2, PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  T_ASSERT(mr != MAP_FAILED);
  if (mr != MAP_FAILED) {
    mr[PGSZ - 1] = 29;
    T_ASSERT_OK(munmap(mr + PGSZ, PGSZ));
    char *grown = mremap(mr, PGSZ, PGSZ * 2, 0);
    T_ASSERT(grown == mr);
    if (grown != MAP_FAILED) {
      T_ASSERT(grown[PGSZ - 1] == 29);
      T_ASSERT(grown[PGSZ] == 0 && grown[PGSZ * 2 - 1] == 0);
      T_ASSERT_OK(munmap(grown, PGSZ * 2));
    } else {
      T_ASSERT_OK(munmap(mr, PGSZ));
    }
  }

  T_BEGIN("mremap/maymove-preserves-data");
  mr = mmap(NULL, PGSZ * 2, PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  T_ASSERT(mr != MAP_FAILED);
  if (mr != MAP_FAILED) {
    mr[0] = 31;
    mr[PGSZ - 1] = 47;
    T_ASSERT_OK(mprotect(mr, PGSZ, PROT_READ));
    char *blocker = mmap(mr + PGSZ, PGSZ, PROT_NONE,
                         MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    T_ASSERT(blocker == mr + PGSZ);
    char *moved = mremap(mr, PGSZ, PGSZ * 2, MREMAP_MAYMOVE);
    T_ASSERT(moved != MAP_FAILED && moved != mr);
    if (moved != MAP_FAILED) {
      T_ASSERT(moved[0] == 31 && moved[PGSZ - 1] == 47);
      T_ASSERT(moved[PGSZ] == 0 && moved[PGSZ * 2 - 1] == 0);
      T_ASSERT_OK(munmap(moved, PGSZ * 2));
    } else {
      T_ASSERT_OK(munmap(mr, PGSZ));
    }
    T_ASSERT_OK(munmap(blocker, PGSZ));
  }

  T_BEGIN("mremap/fixed-move");
  mr = mmap(NULL, PGSZ, PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  char *target = mmap(NULL, PGSZ * 2, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  T_ASSERT(mr != MAP_FAILED && target != MAP_FAILED);
  if (mr != MAP_FAILED && target != MAP_FAILED) {
    mr[123] = 61;
    char *moved = mremap(mr, PGSZ, PGSZ * 2,
                         MREMAP_MAYMOVE | MREMAP_FIXED, target);
    T_ASSERT(moved == target);
    if (moved != MAP_FAILED) {
      T_ASSERT(moved[123] == 61 && moved[PGSZ] == 0);
      T_ASSERT_OK(munmap(moved, PGSZ * 2));
    } else {
      T_ASSERT_OK(munmap(mr, PGSZ));
      T_ASSERT_OK(munmap(target, PGSZ * 2));
    }
  } else {
    if (mr != MAP_FAILED) T_ASSERT_OK(munmap(mr, PGSZ));
    if (target != MAP_FAILED) T_ASSERT_OK(munmap(target, PGSZ * 2));
  }

  T_BEGIN("mremap/invalid-arguments");
  mr = mmap(NULL, PGSZ, PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  T_ASSERT(mr != MAP_FAILED);
  if (mr != MAP_FAILED) {
    T_ASSERT_ERRNO(mremap(mr + 1, PGSZ, PGSZ, 0), EINVAL);
    T_ASSERT_ERRNO(mremap(mr, PGSZ, 0, 0), EINVAL);
    T_ASSERT_ERRNO(mremap(mr, PGSZ, PGSZ, 0x40000000), EINVAL);
    T_ASSERT_ERRNO(mremap(mr, PGSZ, PGSZ, MREMAP_FIXED, mr), EINVAL);
    T_ASSERT_OK(munmap(mr, PGSZ));
  }

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
    T_ASSERT_EQ(write(fd, buf, PGSZ), PGSZ);
    T_ASSERT_EQ(write(fd, buf, PGSZ), PGSZ);
    char *fm = mmap(NULL, PGSZ * 2, PROT_READ, MAP_PRIVATE, fd, 0);
    T_ASSERT(fm != MAP_FAILED);
    if (fm != MAP_FAILED) {
      T_ASSERT_EQ(memcmp(fm, buf, PGSZ), 0);
      char *shrunk = mremap(fm, PGSZ * 2, PGSZ, 0);
      T_ASSERT(shrunk == fm);
      if (shrunk != MAP_FAILED) {
        T_ASSERT_EQ(memcmp(shrunk, buf, PGSZ), 0);
        char *grown = mremap(shrunk, PGSZ, PGSZ * 2, 0);
        T_ASSERT(grown == shrunk);
        if (grown != MAP_FAILED) {
          T_ASSERT_EQ(memcmp(grown + PGSZ, buf, PGSZ), 0);
          char *grown_again = mremap(grown, PGSZ * 2, PGSZ * 3, 0);
          T_ASSERT(grown_again == grown);
          if (grown_again != MAP_FAILED) {
            T_ASSERT_EQ(memcmp(grown_again + PGSZ * 2, buf, PGSZ), 0);
            T_ASSERT_OK(munmap(grown_again, PGSZ * 3));
          } else {
            T_ASSERT_OK(munmap(grown, PGSZ * 2));
          }
        } else {
          T_ASSERT_OK(munmap(shrunk, PGSZ));
        }
      } else {
        T_ASSERT_OK(munmap(fm, PGSZ * 2));
      }
    }
    /* shared writable file mapping persists to disk */
    T_BEGIN("mmap/file-shared-persist");
    fm = mmap(NULL, PGSZ, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    T_ASSERT(fm != MAP_FAILED);
    if (fm != MAP_FAILED) {
      T_ASSERT_ERRNO(msync(fm + 1, PGSZ - 1, MS_SYNC), EINVAL);
      T_ASSERT_ERRNO(msync(fm, PGSZ, MS_SYNC | MS_ASYNC), EINVAL);
      fm[0] = (char)0xAA;
      T_ASSERT_OK(msync(fm, PGSZ, MS_SYNC));
      fm[1] = (char)0xBB;
      T_ASSERT_OK(mprotect(fm, PGSZ, PROT_NONE));
      T_ASSERT_OK(munmap(fm, PGSZ));
      char rb[2];
      T_ASSERT_EQ(pread(fd, rb, sizeof(rb), 0), sizeof(rb));
      T_ASSERT(rb[0] == (char)0xAA && rb[1] == (char)0xBB);
    }

    T_BEGIN("mmap/file-shared-low-host-fd");
    {
      int saved_stdin = dup(STDIN_FILENO);
      T_ASSERT(saved_stdin >= 0);
      if (saved_stdin >= 0) {
        T_ASSERT_OK(close(STDIN_FILENO));
        fm = mmap(NULL, PGSZ, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        T_ASSERT(fm != MAP_FAILED);
        if (fm != MAP_FAILED) {
          fm[2] = (char)0xCC;
          T_ASSERT_OK(munmap(fm, PGSZ));
        }
        T_ASSERT_EQ(dup2(saved_stdin, STDIN_FILENO), STDIN_FILENO);
        T_ASSERT_OK(close(saved_stdin));
        if (fm != MAP_FAILED) {
          char rb;
          T_ASSERT_EQ(pread(fd, &rb, 1, 2), 1);
          T_ASSERT(rb == (char)0xCC);
        }
      }
    }

    T_BEGIN("mremap/file-private-move-after-close");
    fm = mmap(NULL, PGSZ * 2, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    T_ASSERT(fm != MAP_FAILED);
    if (fm != MAP_FAILED) {
      fm[0] = (char)0x55;
      char *blocker = mmap(fm + PGSZ * 2, PGSZ, PROT_NONE,
                           MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
      T_ASSERT(blocker == fm + PGSZ * 2);
      T_ASSERT_OK(close(fd));
      fd = -1;
      char *moved = mremap(fm, PGSZ * 2, PGSZ * 3, MREMAP_MAYMOVE);
      T_ASSERT(moved != MAP_FAILED && moved != fm);
      if (moved != MAP_FAILED) {
        T_ASSERT(moved[0] == (char)0x55);
        T_ASSERT_EQ(memcmp(moved + PGSZ * 2, buf, PGSZ), 0);
        T_ASSERT_OK(munmap(moved, PGSZ * 3));
      } else {
        T_ASSERT_OK(munmap(fm, PGSZ * 2));
      }
      T_ASSERT_OK(munmap(blocker, PGSZ));
    }
    if (fd >= 0) close(fd);
    fd = open("t_mmap_tmp.bin", O_RDWR);
    T_ASSERT(fd >= 0);
    if (fd >= 0) {
      char rb;
      T_ASSERT_EQ(pread(fd, &rb, 1, 0), 1);
      T_ASSERT(rb == (char)0xAA); /* private dirty byte was not written back */
    }

    T_BEGIN("mremap/file-shared-move-writeback");
    fm = mmap(NULL, PGSZ * 2, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    T_ASSERT(fm != MAP_FAILED);
    if (fm != MAP_FAILED) {
      char *blocker = mmap(fm + PGSZ * 2, PGSZ, PROT_NONE,
                           MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
      T_ASSERT(blocker == fm + PGSZ * 2);
      T_ASSERT_OK(close(fd));
      fd = -1;
      char *moved = mremap(fm, PGSZ * 2, PGSZ * 3, MREMAP_MAYMOVE);
      T_ASSERT(moved != MAP_FAILED && moved != fm);
      if (moved != MAP_FAILED) {
        T_ASSERT_EQ(memcmp(moved + PGSZ * 2, buf, PGSZ), 0);
        moved[0] = (char)0x66;
        moved[PGSZ * 2 + 17] = (char)0x77;
        T_ASSERT_OK(msync(moved, PGSZ * 3, MS_SYNC));
        T_ASSERT_OK(munmap(moved, PGSZ * 3));
      } else {
        T_ASSERT_OK(munmap(fm, PGSZ * 2));
      }
      T_ASSERT_OK(munmap(blocker, PGSZ));
    }
    if (fd >= 0) close(fd);
    fd = open("t_mmap_tmp.bin", O_RDONLY);
    T_ASSERT(fd >= 0);
    if (fd >= 0) {
      char rb[2];
      T_ASSERT_EQ(pread(fd, rb, 1, 0), 1);
      T_ASSERT(rb[0] == (char)0x66);
      T_ASSERT_EQ(pread(fd, rb + 1, 1, PGSZ * 2 + 17), 1);
      T_ASSERT(rb[1] == (char)0x77);
    }

    T_BEGIN("mremap/file-fixed-replaces-shared-target");
    int targetfd = open("t_mmap_target.bin",
                        O_RDWR | O_CREAT | O_TRUNC, 0600);
    T_ASSERT(targetfd >= 0);
    if (fd >= 0 && targetfd >= 0) {
      T_ASSERT_EQ(write(targetfd, buf, PGSZ), PGSZ);
      char *source = mmap(NULL, PGSZ, PROT_READ, MAP_PRIVATE, fd, 0);
      char *target = mmap(NULL, PGSZ, PROT_READ | PROT_WRITE,
                          MAP_SHARED, targetfd, 0);
      T_ASSERT(source != MAP_FAILED && target != MAP_FAILED);
      if (source != MAP_FAILED && target != MAP_FAILED) {
        target[0] = (char)0x22;
        char *moved = mremap(source, PGSZ, PGSZ,
                             MREMAP_MAYMOVE | MREMAP_FIXED, target);
        T_ASSERT(moved == target);
        if (moved != MAP_FAILED) {
          char rb;
          T_ASSERT(moved[0] == (char)0x66);
          T_ASSERT_EQ(pread(targetfd, &rb, 1, 0), 1);
          T_ASSERT(rb == (char)0x22);
          T_ASSERT_OK(munmap(moved, PGSZ));
        } else {
          T_ASSERT_OK(munmap(source, PGSZ));
          T_ASSERT_OK(munmap(target, PGSZ));
        }
      } else {
        if (source != MAP_FAILED) T_ASSERT_OK(munmap(source, PGSZ));
        if (target != MAP_FAILED) T_ASSERT_OK(munmap(target, PGSZ));
      }
    }
    if (targetfd >= 0) close(targetfd);
    unlink("t_mmap_target.bin");
    close(fd);
    unlink("t_mmap_tmp.bin");
  }

  T_BEGIN("mmap/file-shared-many-writeback");
  {
    enum { MAP_COUNT = 160 };
    char *maps[MAP_COUNT];
    int stressfd;
    int mapped = 0;
    int persisted = 1;
    memset(maps, 0, sizeof(maps));
    stressfd = open("t_mmap_many.bin",
                    O_RDWR | O_CREAT | O_TRUNC, 0600);
    T_ASSERT(stressfd >= 0);
    if (stressfd >= 0) {
      T_ASSERT_OK(ftruncate(stressfd, (off_t)MAP_COUNT * PGSZ));
      for (int i = 0; i < MAP_COUNT; ++i) {
        maps[i] = mmap(NULL, PGSZ, PROT_READ | PROT_WRITE, MAP_SHARED,
                       stressfd, (off_t)i * PGSZ);
        if (maps[i] == MAP_FAILED) break;
        maps[i][0] = (char)(i + 1);
        ++mapped;
      }
      T_ASSERT_EQ(mapped, MAP_COUNT);
      for (int i = 0; i < mapped; ++i) {
        if (munmap(maps[i], PGSZ) == -1) persisted = 0;
      }
      for (int i = 0; i < MAP_COUNT; ++i) {
        unsigned char byte = 0;
        if (pread(stressfd, &byte, 1, (off_t)i * PGSZ) != 1 ||
            byte != (unsigned char)(i + 1)) {
          persisted = 0;
          break;
        }
      }
      T_ASSERT(persisted);
      T_ASSERT_OK(close(stressfd));
    }
    unlink("t_mmap_many.bin");
  }

  T_BEGIN("mmap/file-shared-offset-gt4GB");
  {
    const off_t high = (off_t)4 * 1024 * 1024 * 1024 + PGSZ;
    unsigned char low_byte = 0x31;
    unsigned char high_byte = 0x47;
    unsigned char next_byte = 0x6b;
    int largefd = open("t_mmap_large_offset.bin",
                       O_RDWR | O_CREAT | O_TRUNC, 0600);
    T_ASSERT(largefd >= 0);
    if (largefd >= 0) {
      T_ASSERT_EQ(lseek(largefd, 123, SEEK_SET), 123);
      T_ASSERT_OK(ftruncate(largefd, high + PGSZ * 2));
      T_ASSERT_EQ(lseek(largefd, 0, SEEK_CUR), 123);
      T_ASSERT_EQ(pwrite(largefd, &low_byte, 1, PGSZ), 1);
      T_ASSERT_EQ(pwrite(largefd, &high_byte, 1, high), 1);
      T_ASSERT_EQ(pwrite(largefd, &next_byte, 1, high + PGSZ), 1);
      unsigned char *large =
          mmap(NULL, PGSZ, PROT_READ | PROT_WRITE, MAP_SHARED, largefd, high);
      T_ASSERT(large != MAP_FAILED);
      if (large != MAP_FAILED) {
        T_ASSERT(large[0] == high_byte);
        unsigned char *grown =
            mremap(large, PGSZ, PGSZ * 2, MREMAP_MAYMOVE);
        T_ASSERT(grown != MAP_FAILED);
        if (grown != MAP_FAILED) {
          large = grown;
          T_ASSERT(large[PGSZ] == next_byte);
          large[0] = 0x59;
          T_ASSERT_OK(msync(large, PGSZ * 2, MS_SYNC));
          T_ASSERT_OK(munmap(large, PGSZ * 2));
          T_ASSERT_EQ(pread(largefd, &low_byte, 1, PGSZ), 1);
          T_ASSERT(low_byte == 0x31);
          T_ASSERT_EQ(pread(largefd, &high_byte, 1, high), 1);
          T_ASSERT(high_byte == 0x59);
        } else {
          T_ASSERT_OK(munmap(large, PGSZ));
        }
      }
      T_ASSERT_OK(close(largefd));
    }
    unlink("t_mmap_large_offset.bin");
  }

  /* --- invalid combos must fail --- */
  T_BEGIN("mmap/file-badfd-EBADF");
  T_ASSERT_ERRNO(mmap(NULL, PGSZ, PROT_READ, MAP_PRIVATE, 9999, 0), EBADF);

  T_BEGIN("mmap/len-zero-EINVAL");
  T_ASSERT_ERRNO(mmap(NULL, 0, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0), EINVAL);

  T_BEGIN("mmap/anon-fd-ignored");
  /* real Linux: fd is ignored when MAP_ANONYMOUS is set */
  {
    int d = open("/dev/null", O_RDONLY);
    errno = 0;
    void *r = mmap(NULL, PGSZ, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, d, 0);
    if (r != MAP_FAILED) {
      ((char *)r)[0] = 1;
      wtest_pass++, printf("PASS %s\n", wtest_cur);
      munmap(r, PGSZ);
    } else {
      wtest_fail++; printf("FAIL %s: anon+fd=%d => %p errno=%d\n",
                            wtest_cur, d, r, errno);
    }
    close(d);
  }

  return WTEST_END();
}
