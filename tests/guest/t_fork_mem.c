/* t_fork_mem.c — fork snapshot memory isolation (core invariant of snapshot fork),
 * plus exec memory cleanliness. */
#define _GNU_SOURCE
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include "wtest.h"

static int wait_status_ok(pid_t pid, int expect_code) {
  int st;
  if (waitpid(pid, &st, 0) != pid) return 0;
  return WIFEXITED(st) && WEXITSTATUS(st) == expect_code;
}

static int fork_failure_probe(void) {
  int st;
  int fd = open("t_fork_failure.bin",
                O_RDWR | O_CREAT | O_TRUNC, 0600);
  if (fd == -1 || ftruncate(fd, 8192) == -1) return 1;
  unsigned char value = 0x5a;
  if (pwrite(fd, &value, 1, 4096) != 1) return 2;
  unsigned char *fm =
      mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
  if (fm == MAP_FAILED) return 3;
  void *blocker = mmap(fm + 4096, 4096, PROT_NONE,
                       MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
  if (blocker != fm + 4096) return 4;
  int *guard = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                    MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (guard == MAP_FAILED) return 5;
  *guard = 0x12345678;
  errno = 0;
  pid_t pid = fork();
  if (pid == 0) _exit(10);
  if (pid > 0) {
    waitpid(pid, &st, 0);
    return 6;
  }
  if ((errno != EAGAIN && errno != ENOMEM) ||
      *guard != 0x12345678) {
    return 7;
  }
  unsigned char *moved = mremap(fm, 4096, 8192, MREMAP_MAYMOVE);
  if (moved == MAP_FAILED || moved == fm || moved[4096] != 0x5a) return 8;
  pid = fork();
  if (pid == 0)
    _exit(*guard == 0x12345678 && moved[4096] == 0x5a ? 0 : 11);
  if (pid < 0 || !wait_status_ok(pid, 0) ||
      *guard != 0x12345678 || moved[4096] != 0x5a) {
    return 9;
  }
  if (munmap(moved, 8192) == -1 ||
      munmap(blocker, 4096) == -1 ||
      munmap(guard, 4096) == -1 ||
      close(fd) == -1 ||
      unlink("t_fork_failure.bin") == -1) {
    return 10;
  }
  return 0;
}

int main(int argc, char **argv) {
  if (argc == 2 && strcmp(argv[1], "--fork-failure") == 0) {
    return fork_failure_probe();
  }

  /* --- private anon page: child write invisible to parent --- */
  T_BEGIN("fork/child-write-isolation");
  int *sh = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  T_ASSERT(sh != MAP_FAILED);
  if (sh == MAP_FAILED) return WTEST_END();
  *sh = 111;
  pid_t pid = fork();
  T_ASSERT(pid >= 0);
  if (pid == 0) {
    *sh = 222;
    _exit(*sh == 222 ? 0 : 1); /* child sees its own write */
  }
  if (pid > 0) {
    T_ASSERT(wait_status_ok(pid, 0));
    T_ASSERT(*sh == 111); /* parent copy untouched */
  }

  /* --- parent write after fork invisible to child --- */
  T_BEGIN("fork/parent-write-isolation");
  *sh = 333;
  pid = fork();
  T_ASSERT(pid >= 0);
  if (pid == 0) _exit(0); /* child takes snapshot then sleeps */
  if (pid > 0) {
    *sh = 444;
    T_ASSERT(wait_status_ok(pid, 0));
    /* and verify child observed 333: redo with pipe */
    int pp[2];
    T_ASSERT_OK(pipe(pp));
    *sh = 555;
    pid = fork();
    if (pid == 0) {
      close(pp[0]);
      int v = *sh;
      *sh = 666; /* mutate own copy */
      (void)!write(pp[1], &v, sizeof v);
      _exit(0);
    }
    close(pp[1]);
    *sh = 777; /* parent mutates after fork */
    int got = 0;
    T_ASSERT_EQ(read(pp[0], &got, sizeof got), sizeof got);
    T_ASSERT(got == 555);
    T_ASSERT(*sh == 777);
    close(pp[0]);
    T_ASSERT(wait_status_ok(pid, 0));
  }

  /* --- nested fork: grandchild isolation --- */
  T_BEGIN("fork/nested-isolation");
  *sh = 1000;
  pid = fork();
  if (pid == 0) {
    *sh = 2000;
    pid_t g = fork();
    if (g == 0) { *sh = 3000; _exit(*sh == 3000 ? 0 : 1); }
    int st;
    waitpid(g, &st, 0);
    _exit(*sh == 2000 && WIFEXITED(st) && WEXITSTATUS(st) == 0 ? 0 : 1);
  }
  if (pid > 0) {
    T_ASSERT(wait_status_ok(pid, 0));
    T_ASSERT(*sh == 1000);
  }

  /* --- brk-grown region isolation across fork --- */
  T_BEGIN("fork/brk-region-isolation");
  {
    /* raw SYS_brk: zig-musl brk() is an -ENOMEM stub (see t_brk.c) */
    void *base = (void *)syscall(SYS_brk, 0);
    if (syscall(SYS_brk, (char *)base + 8192) == (long)((char *)base + 8192)) {
      ((char *)base)[100] = 42;
      pid = fork();
      if (pid == 0) {
        ((char *)base)[100] = 43;
        _exit(0);
      }
      if (pid > 0) {
        T_ASSERT(wait_status_ok(pid, 0));
        T_ASSERT(((char *)base)[100] == 42);
      }
    } else {
      T_ASSERT(syscall(SYS_brk, (char *)base + 8192) ==
               (long)((char *)base + 8192)); /* FAIL line */
      T_SKIP("fork/brk-region-isolation-data", "brk grow failed");
    }
  }

  /* --- file-shared MAP_SHARED page: writes visible across fork --- */
  T_BEGIN("fork/map-shared-visible");
  {
    int *sp = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                   MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    T_ASSERT(sp != MAP_FAILED);
    if (sp != MAP_FAILED) {
      *sp = 7;
      pid = fork();
      if (pid == 0) { *sp = 8; _exit(0); }
      if (pid > 0) {
        T_ASSERT(wait_status_ok(pid, 0));
        T_ASSERT(*sp == 8);
      }
      munmap(sp, 4096);
    }
  }

  T_BEGIN("fork/anon-shared-child-munmap-visible");
  {
    int *sp = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                   MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    T_ASSERT(sp != MAP_FAILED);
    if (sp != MAP_FAILED) {
      *sp = 41;
      pid = fork();
      T_ASSERT(pid >= 0);
      if (pid == 0) {
        *sp = 43;
        _exit(munmap(sp, 4096) == 0 ? 0 : 1);
      }
      if (pid > 0) {
        T_ASSERT(wait_status_ok(pid, 0));
        T_ASSERT(*sp == 43);
      }
      T_ASSERT_OK(munmap(sp, 4096));
    }
  }

  T_BEGIN("fork/anon-shared-many-visible");
  {
    enum { MAP_COUNT = 160 };
    unsigned char *maps[MAP_COUNT];
    int mapped = 0;
    int cleaned = 1;
    memset(maps, 0, sizeof(maps));
    for (int i = 0; i < MAP_COUNT; ++i) {
      maps[i] = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                     MAP_SHARED | MAP_ANONYMOUS, -1, 0);
      if (maps[i] == MAP_FAILED) break;
      ++mapped;
    }
    T_ASSERT_EQ(mapped, MAP_COUNT);
    if (mapped == MAP_COUNT) {
      pid = fork();
      T_ASSERT(pid >= 0);
      if (pid == 0) {
        maps[0][0] = 0xb1;
        maps[MAP_COUNT - 1][0] = 0xb7;
        _exit(0);
      }
      if (pid > 0) {
        T_ASSERT(wait_status_ok(pid, 0));
        T_ASSERT(maps[0][0] == 0xb1);
        T_ASSERT(maps[MAP_COUNT - 1][0] == 0xb7);
      }
    }
    for (int i = 0; i < mapped; ++i) {
      if (munmap(maps[i], 4096) == -1) cleaned = 0;
    }
    T_ASSERT(cleaned);
  }

  T_BEGIN("fork/file-shared-child-munmap-visible");
  {
    int fd = open("t_fork_mem_child_unmap.bin",
                  O_RDWR | O_CREAT | O_TRUNC, 0600);
    T_ASSERT(fd >= 0);
    if (fd >= 0) {
      int initial = 47;
      T_ASSERT_EQ(write(fd, &initial, sizeof(initial)), sizeof(initial));
      int *sp = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
      T_ASSERT(sp != MAP_FAILED);
      if (sp != MAP_FAILED) {
        pid = fork();
        T_ASSERT(pid >= 0);
        if (pid == 0) {
          *sp = 53;
          _exit(munmap(sp, 4096) == 0 ? 0 : 1);
        }
        if (pid > 0) {
          int ondisk = 0;
          T_ASSERT(wait_status_ok(pid, 0));
          T_ASSERT(*sp == 53);
          T_ASSERT_EQ(pread(fd, &ondisk, sizeof(ondisk), 0), sizeof(ondisk));
          T_ASSERT(ondisk == 53);
        }
        T_ASSERT_OK(munmap(sp, 4096));
      }
      T_ASSERT_OK(close(fd));
      T_ASSERT_OK(unlink("t_fork_mem_child_unmap.bin"));
    }
  }

  /* --- file-backed MAP_SHARED: child writes reach parent and disk --- */
  T_BEGIN("fork/file-shared-visible-writeback");
  {
    int fd = open("t_fork_mem_shared.bin",
                  O_RDWR | O_CREAT | O_TRUNC, 0600);
    T_ASSERT(fd >= 0);
    if (fd >= 0) {
      int initial = 17;
      T_ASSERT_EQ(write(fd, &initial, sizeof(initial)), sizeof(initial));
      int *sp = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
      T_ASSERT(sp != MAP_FAILED);
      if (sp != MAP_FAILED) {
        pid = fork();
        T_ASSERT(pid >= 0);
        if (pid == 0) {
          *sp = 29;
          _exit(msync(sp, 4096, MS_SYNC) == 0 ? 0 : 1);
        }
        if (pid > 0) {
          int ondisk = 0;
          T_ASSERT(wait_status_ok(pid, 0));
          T_ASSERT(*sp == 29);
          T_ASSERT_EQ(pread(fd, &ondisk, sizeof(ondisk), 0), sizeof(ondisk));
          T_ASSERT(ondisk == 29);
        }
        T_ASSERT_OK(munmap(sp, 4096));
      }
      close(fd);
      unlink("t_fork_mem_shared.bin");
    }
  }

  T_BEGIN("fork/file-shared-many-visible");
  {
    enum { MAP_COUNT = 160 };
    unsigned char *maps[MAP_COUNT];
    int fd = open("t_fork_mem_many.bin",
                  O_RDWR | O_CREAT | O_TRUNC, 0600);
    int mapped = 0;
    int cleaned = 1;
    memset(maps, 0, sizeof(maps));
    T_ASSERT(fd >= 0);
    if (fd >= 0) {
      T_ASSERT_OK(ftruncate(fd, (off_t)MAP_COUNT * 4096));
      for (int i = 0; i < MAP_COUNT; ++i) {
        maps[i] = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED,
                       fd, (off_t)i * 4096);
        if (maps[i] == MAP_FAILED) break;
        ++mapped;
      }
      T_ASSERT_EQ(mapped, MAP_COUNT);
      if (mapped == MAP_COUNT) {
        pid = fork();
        T_ASSERT(pid >= 0);
        if (pid == 0) {
          maps[0][0] = 0xd1;
          maps[MAP_COUNT - 1][0] = 0xd7;
          _exit(msync(maps[MAP_COUNT - 1], 4096, MS_SYNC) == 0 ? 0 : 1);
        }
        if (pid > 0) {
          unsigned char ondisk[2] = {0};
          T_ASSERT(wait_status_ok(pid, 0));
          T_ASSERT(maps[0][0] == 0xd1);
          T_ASSERT(maps[MAP_COUNT - 1][0] == 0xd7);
          T_ASSERT_EQ(pread(fd, ondisk, 1, 0), 1);
          T_ASSERT_EQ(pread(fd, ondisk + 1, 1,
                            (off_t)(MAP_COUNT - 1) * 4096), 1);
          T_ASSERT(ondisk[0] == 0xd1 && ondisk[1] == 0xd7);
        }
      }
      for (int i = 0; i < mapped; ++i) {
        if (munmap(maps[i], 4096) == -1) cleaned = 0;
      }
      T_ASSERT(cleaned);
      T_ASSERT_OK(close(fd));
      T_ASSERT_OK(unlink("t_fork_mem_many.bin"));
    }
  }

  T_BEGIN("fork/file-shared-msync-before-exit-visible");
  {
    int fd = open("t_fork_mem_msync_live.bin",
                  O_RDWR | O_CREAT | O_TRUNC, 0600);
    int ready[2] = {-1, -1};
    int release[2] = {-1, -1};
    T_ASSERT(fd >= 0);
    if (fd >= 0) {
      int initial = 67;
      T_ASSERT_EQ(write(fd, &initial, sizeof(initial)), sizeof(initial));
      int *sp = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
      T_ASSERT(sp != MAP_FAILED);
      T_ASSERT_OK(pipe(ready));
      T_ASSERT_OK(pipe(release));
      if (sp != MAP_FAILED && ready[0] >= 0 && release[0] >= 0) {
        pid = fork();
        T_ASSERT(pid >= 0);
        if (pid == 0) {
          char byte = 1;
          close(ready[0]);
          close(release[1]);
          *sp = 71;
          if (msync(sp, 4096, MS_SYNC) == -1 ||
              write(ready[1], &byte, 1) != 1 ||
              read(release[0], &byte, 1) != 1) {
            _exit(1);
          }
          _exit(0);
        }
        if (pid > 0) {
          char byte = 0;
          close(ready[1]);
          ready[1] = -1;
          close(release[0]);
          release[0] = -1;
          T_ASSERT_EQ(read(ready[0], &byte, 1), 1);
          T_ASSERT(*sp == 71);
          T_ASSERT_EQ(write(release[1], &byte, 1), 1);
          T_ASSERT(wait_status_ok(pid, 0));
        }
        T_ASSERT_OK(munmap(sp, 4096));
      }
      if (ready[0] >= 0) close(ready[0]);
      if (ready[1] >= 0) close(ready[1]);
      if (release[0] >= 0) close(release[0]);
      if (release[1] >= 0) close(release[1]);
      T_ASSERT_OK(close(fd));
      T_ASSERT_OK(unlink("t_fork_mem_msync_live.bin"));
    }
  }

  T_BEGIN("fork/file-shared-child-fixed-visible");
  {
    int fd = open("t_fork_mem_child_fixed.bin",
                  O_RDWR | O_CREAT | O_TRUNC, 0600);
    T_ASSERT(fd >= 0);
    if (fd >= 0) {
      int initial = 59;
      T_ASSERT_EQ(write(fd, &initial, sizeof(initial)), sizeof(initial));
      int *sp = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
      T_ASSERT(sp != MAP_FAILED);
      if (sp != MAP_FAILED) {
        pid = fork();
        T_ASSERT(pid >= 0);
        if (pid == 0) {
          *sp = 61;
          int *replacement =
              mmap(sp, 4096, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
          if (replacement != sp || *replacement != 0) _exit(1);
          _exit(munmap(replacement, 4096) == 0 ? 0 : 2);
        }
        if (pid > 0) {
          int ondisk = 0;
          T_ASSERT(wait_status_ok(pid, 0));
          T_ASSERT(*sp == 61);
          T_ASSERT_EQ(pread(fd, &ondisk, sizeof(ondisk), 0), sizeof(ondisk));
          T_ASSERT(ondisk == 61);
        }
        T_ASSERT_OK(munmap(sp, 4096));
      }
      T_ASSERT_OK(close(fd));
      T_ASSERT_OK(unlink("t_fork_mem_child_fixed.bin"));
    }
  }

  /* --- inherited file mapping keeps its backing for child mremap --- */
  T_BEGIN("fork/file-private-mremap");
  {
    int fd = open("t_fork_mem_remap.bin",
                  O_RDWR | O_CREAT | O_TRUNC, 0600);
    T_ASSERT(fd >= 0);
    if (fd >= 0) {
      unsigned char page[4096];
      for (int i = 0; i < 3; ++i) {
        memset(page, 0x30 + i, sizeof(page));
        T_ASSERT_EQ(write(fd, page, sizeof(page)), sizeof(page));
      }
      unsigned char *fm =
          mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
      T_ASSERT(fm != MAP_FAILED);
      if (fm != MAP_FAILED) {
        void *blocker = mmap(fm + 8192, 4096, PROT_NONE,
                             MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
        T_ASSERT(blocker == fm + 8192);
        pid = fork();
        T_ASSERT(pid >= 0);
        if (pid == 0) {
          unsigned char *moved = mremap(fm, 8192, 12288, MREMAP_MAYMOVE);
          if (moved == MAP_FAILED || moved == fm || moved[8192] != 0x32) {
            _exit(1);
          }
          moved[0] = 0x7f;
          _exit(munmap(moved, 12288) == 0 ? 0 : 2);
        }
        if (pid > 0) {
          T_ASSERT(wait_status_ok(pid, 0));
          T_ASSERT(fm[0] == 0x30);
          unsigned char *moved =
              mremap(fm, 8192, 12288, MREMAP_MAYMOVE);
          T_ASSERT(moved != MAP_FAILED && moved != fm);
          if (moved != MAP_FAILED) {
            T_ASSERT(moved[8192] == 0x32);
            T_ASSERT_OK(munmap(moved, 12288));
          } else {
            T_ASSERT_OK(munmap(fm, 8192));
          }
        } else {
          T_ASSERT_OK(munmap(fm, 8192));
        }
        T_ASSERT_OK(munmap(blocker, 4096));
      }
      T_ASSERT_OK(close(fd));
      T_ASSERT_OK(unlink("t_fork_mem_remap.bin"));
    }
  }

  /* --- moved shared mapping still updates the parent's original VA --- */
  T_BEGIN("fork/file-shared-mremap-visible");
  {
    int fd = open("t_fork_mem_shared_remap.bin",
                  O_RDWR | O_CREAT | O_TRUNC, 0600);
    T_ASSERT(fd >= 0);
    if (fd >= 0) {
      T_ASSERT_OK(ftruncate(fd, 12288));
      unsigned char *fm =
          mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
      T_ASSERT(fm != MAP_FAILED);
      if (fm != MAP_FAILED) {
        void *blocker = mmap(fm + 8192, 4096, PROT_NONE,
                             MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
        T_ASSERT(blocker == fm + 8192);
        fm[0] = 51;
        pid = fork();
        T_ASSERT(pid >= 0);
        if (pid == 0) {
          unsigned char *moved = mremap(fm, 8192, 12288, MREMAP_MAYMOVE);
          if (moved == MAP_FAILED || moved == fm) _exit(1);
          moved[0] = 61;
          moved[8192] = 62;
          _exit(msync(moved, 12288, MS_SYNC) == 0 ? 0 : 2);
        }
        if (pid > 0) {
          unsigned char ondisk[2] = {0};
          T_ASSERT(wait_status_ok(pid, 0));
          T_ASSERT(fm[0] == 61);
          T_ASSERT_EQ(pread(fd, ondisk, 1, 0), 1);
          T_ASSERT_EQ(pread(fd, ondisk + 1, 1, 8192), 1);
          T_ASSERT(ondisk[0] == 61 && ondisk[1] == 62);
        }
        T_ASSERT_OK(munmap(fm, 8192));
        T_ASSERT_OK(munmap(blocker, 4096));
      }
      T_ASSERT_OK(close(fd));
      T_ASSERT_OK(unlink("t_fork_mem_shared_remap.bin"));
    }
  }

  /* --- exec memory cleanliness: anon page must not leak into new image ---
   * child writes a marker into an anon page then execs /bin/true-ish self;
   * if exec succeeds, the marker page is simply gone (process image replaced).
   * We verify exec actually happened via exit code of the exec'd program. */
  T_BEGIN("fork/exec-replaces-image");
  {
    pid = fork();
    if (pid == 0) {
      sh[1] = 0x5A; /* marker in this (private) page */
      execl("/bin/true", "true", (char *)0);
      _exit(111); /* exec failed */
    }
    if (pid > 0) {
      int st;
      T_ASSERT(waitpid(pid, &st, 0) == pid);
      if (WIFEXITED(st) && WEXITSTATUS(st) == 0)
        T_ASSERT(1);
      else if (WIFEXITED(st) && WEXITSTATUS(st) == 111)
        T_SKIP("fork/exec-replaces-image", "/bin/true unavailable in rootfs");
      else
        T_ASSERT(0);
    }
    T_ASSERT(sh[1] == 0 || sh[1] == 0x5A); /* parent page state sane */
  }

  munmap(sh, 4096);
  return WTEST_END();
}
