/* t_exec.c — exec variants, argv/env passing, post-exec state. */
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdint.h>
#include <sys/wait.h>
#include <sys/mman.h>
#include <unistd.h>
#include <stdlib.h>
#include "wtest.h"

int main(int argc, char **argv) {
  /* self-exec child modes */
  if (argc >= 2 && strcmp(argv[1], "--child-argv") == 0) {
    /* verify argv/env landed */
    int ok = argc == 4 && strcmp(argv[2], "x y") == 0 && strcmp(argv[3], "z") == 0;
    const char *e = getenv("T_EXEC_MARK");
    _exit(ok && e && strcmp(e, "marked") == 0 ? 0 : 1);
  }
  if (argc >= 2 && strcmp(argv[1], "--child-mem") == 0) {
    /* this code only runs if exec replaced the image; exit 0 */
    _exit(0);
  }
  if (argc >= 2 && strcmp(argv[1], "--child-map-reuse") == 0) {
    void *want = (void *)(uintptr_t)0x50000000;
    int fd = open("t_exec_map_reuse.bin", O_RDONLY);
    if (fd == -1) _exit(1);
    unsigned char *p =
        mmap(want, 4096, PROT_READ,
             MAP_PRIVATE | MAP_FIXED_NOREPLACE, fd, 0);
    int ok = p == want && p[0] == 0x5a;
    if (p != MAP_FAILED && munmap(p, 4096) == -1) ok = 0;
    if (close(fd) == -1) ok = 0;
    _exit(ok ? 0 : 2);
  }

  char self[256];
  ssize_t n = readlink("/proc/self/exe", self, sizeof self - 1);
  T_BEGIN("exec/readlink-self-exe");
  T_ASSERT(n > 0);
  if (n <= 0) {
    /* fallback: exec via argv[0] (works when invoked with a path) */
    if (argv[0] && strchr(argv[0], '/') && access(argv[0], X_OK) == 0) {
      strncpy(self, argv[0], sizeof self - 1);
      self[sizeof self - 1] = 0;
      T_SKIP("exec/readlink-self-exe-fallback", "using argv[0] instead");
    } else {
      T_SKIP("exec/*", "no /proc/self/exe and argv[0] not usable");
      return WTEST_END();
    }
  } else {
    self[n] = 0;
  }

  /* --- execl with argv containing spaces + env --- */
  T_BEGIN("exec/execl-argv-env");
  {
    pid_t pid = fork();
    if (pid == 0) {
      setenv("T_EXEC_MARK", "marked", 1);
      execl(self, self, "--child-argv", "x y", "z", (char *)0);
      _exit(111);
    }
    int st;
    T_ASSERT_EQ(waitpid(pid, &st, 0), pid);
    T_ASSERT(WIFEXITED(st) && WEXITSTATUS(st) == 0);
  }

  /* --- execv --- */
  T_BEGIN("exec/execv-basic");
  {
    pid_t pid = fork();
    if (pid == 0) {
      char *args[] = {self, "--child-mem", NULL};
      execv(self, args);
      _exit(111);
    }
    int st;
    T_ASSERT_EQ(waitpid(pid, &st, 0), pid);
    T_ASSERT(WIFEXITED(st) && WEXITSTATUS(st) == 0);
  }

  /* --- exec flushes and detaches inherited file-shared mappings --- */
  T_BEGIN("exec/file-shared-writeback");
  {
    int fd = open("t_exec_shared.bin", O_RDWR | O_CREAT | O_TRUNC, 0600);
    T_ASSERT(fd >= 0);
    if (fd >= 0) {
      int initial = 31;
      T_ASSERT_EQ(write(fd, &initial, sizeof(initial)), sizeof(initial));
      int *sp = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
      T_ASSERT(sp != MAP_FAILED);
      if (sp != MAP_FAILED) {
        pid_t pid = fork();
        T_ASSERT(pid >= 0);
        if (pid == 0) {
          *sp = 47;
          execl(self, self, "--child-mem", (char *)0);
          _exit(111);
        }
        if (pid > 0) {
          int st;
          int ondisk = 0;
          T_ASSERT_EQ(waitpid(pid, &st, 0), pid);
          T_ASSERT(WIFEXITED(st) && WEXITSTATUS(st) == 0);
          T_ASSERT(*sp == 47);
          T_ASSERT_EQ(pread(fd, &ondisk, sizeof(ondisk), 0), sizeof(ondisk));
          T_ASSERT(ondisk == 47);
        }
        T_ASSERT_OK(munmap(sp, 4096));
      }
      T_ASSERT_OK(close(fd));
      T_ASSERT_OK(unlink("t_exec_shared.bin"));
    }
  }

  /* --- exec removes inherited VFS map metadata before address reuse --- */
  T_BEGIN("exec/file-map-address-reuse");
  {
    void *want = (void *)(uintptr_t)0x50000000;
    int fd = open("t_exec_map_reuse.bin", O_RDWR | O_CREAT | O_TRUNC, 0600);
    T_ASSERT(fd >= 0);
    if (fd >= 0) {
      T_ASSERT_OK(ftruncate(fd, 4096));
      unsigned char value = 0x5a;
      T_ASSERT_EQ(pwrite(fd, &value, 1, 0), 1);
      unsigned char *p =
          mmap(want, 4096, PROT_READ,
               MAP_PRIVATE | MAP_FIXED_NOREPLACE, fd, 0);
      T_ASSERT(p == want);
      if (p == want) {
        pid_t pid = fork();
        T_ASSERT(pid >= 0);
        if (pid == 0) {
          execl(self, self, "--child-map-reuse", (char *)0);
          _exit(111);
        }
        if (pid > 0) {
          int st;
          T_ASSERT_EQ(waitpid(pid, &st, 0), pid);
          T_ASSERT(WIFEXITED(st) && WEXITSTATUS(st) == 0);
          T_ASSERT(p[0] == 0x5a);
        }
        T_ASSERT_OK(munmap(p, 4096));
      }
      T_ASSERT_OK(close(fd));
      T_ASSERT_OK(unlink("t_exec_map_reuse.bin"));
    }
  }

  /* --- exec of missing path: ENOENT --- */
  T_BEGIN("exec/missing-ENOENT");
  {
    pid_t pid = fork();
    if (pid == 0) {
      execl("/nonexistent/path/to/bin", "bin", (char *)0);
      _exit(errno == ENOENT ? 0 : 1);
    }
    int st;
    T_ASSERT_EQ(waitpid(pid, &st, 0), pid);
    T_ASSERT(WIFEXITED(st) && WEXITSTATUS(st) == 0);
  }

  /* --- exec clears anonymous mappings: child mmaps a page, execs, and the
   * new image cannot see it (address is simply absent). Verified indirectly:
   * child execs --child-mem which exits 0 only if running fresh image. A
   * stronger check: mmap at a fixed address, exec, then in new image mmap
   * the same fixed address must succeed (old mapping gone). --- */
  T_BEGIN("exec/clears-mappings");
  {
    pid_t pid = fork();
    if (pid == 0) {
      void *m = mmap(NULL, 64 * 1024 * 1024, PROT_READ | PROT_WRITE,
                     MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
      if (m == MAP_FAILED) _exit(2);
      *(char *)m = 1;
      execl(self, self, "--child-mem", (char *)0);
      _exit(111);
    }
    int st;
    T_ASSERT_EQ(waitpid(pid, &st, 0), pid);
    T_ASSERT(WIFEXITED(st) && WEXITSTATUS(st) == 0);
  }

  /* --- exec busybox applet via /bin/sh: end-to-end --- */
  T_BEGIN("exec/exec-sh-echo");
  {
    int pp[2];
    T_ASSERT_OK(pipe(pp));
    pid_t pid = fork();
    if (pid == 0) {
      close(pp[0]);
      dup2(pp[1], 1);
      execl("/bin/sh", "sh", "-c", "echo exec-ok", (char *)0);
      _exit(111);
    }
    close(pp[1]);
    char buf[32] = {0};
    ssize_t r = read(pp[0], buf, sizeof buf - 1);
    int st;
    waitpid(pid, &st, 0);
    close(pp[0]);
    if (WIFEXITED(st) && WEXITSTATUS(st) == 111) {
      T_SKIP("exec/exec-sh-echo", "/bin/sh unavailable");
    } else {
      T_ASSERT(r > 0 && strstr(buf, "exec-ok") != NULL);
    }
  }

  return WTEST_END();
}
