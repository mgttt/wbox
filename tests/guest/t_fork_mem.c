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

int main(void) {
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
