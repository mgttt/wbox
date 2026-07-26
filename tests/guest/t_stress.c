/* t_stress.c — slow stress cases (skippable via --skip-slow):
 * 100 fork loop, 1000 mmap/munmap loop, large-file checksum. */
#define _GNU_SOURCE
#include <sys/mman.h>
#include <sys/wait.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
#include "wtest.h"

/* 64-bit FNV-1a rolling checksum over a file */
static int checksum(const char *path, unsigned long long *out) {
  int fd = open(path, O_RDONLY);
  if (fd < 0) return -1;
  unsigned long long h = 1469598103934665603ULL;
  char buf[65536];
  ssize_t n;
  while ((n = read(fd, buf, sizeof buf)) > 0)
    for (ssize_t i = 0; i < n; i++) { h ^= (unsigned char)buf[i]; h *= 1099511628211ULL; }
  close(fd);
  if (n < 0) return -1;
  *out = h;
  return 0;
}

int main(void) {
  /* --- 100 sequential forks --- */
  T_BEGIN("stress/fork-x100");
  {
    int ok = 1;
    for (int i = 0; i < 100; i++) {
      pid_t p = fork();
      if (p < 0) { ok = 0; break; }
      if (p == 0) _exit(i & 0x7f);
      int st;
      if (waitpid(p, &st, 0) != p || !WIFEXITED(st) || WEXITSTATUS(st) != (i & 0x7f)) {
        ok = 0;
        break;
      }
    }
    T_ASSERT(ok);
  }

  /* --- 20 concurrent children --- */
  T_BEGIN("stress/fork-concurrent-20");
  {
    int ok = 1;
    for (int i = 0; i < 20; i++) {
      pid_t p = fork();
      if (p < 0) { ok = 0; break; }
      if (p == 0) { usleep(50 * 1000); _exit(0); }
    }
    int got = 0, st;
    while (wait(&st) > 0) if (WIFEXITED(st) && WEXITSTATUS(st) == 0) got++;
    T_ASSERT(ok && got == 20);
  }

  /* --- 1000 mmap/munmap cycles --- */
  T_BEGIN("stress/mmap-x1000");
  {
    int ok = 1;
    for (int i = 0; i < 1000; i++) {
      size_t sz = 4096 * (1 + (i % 16));
      char *p = mmap(NULL, sz, PROT_READ | PROT_WRITE,
                     MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
      if (p == MAP_FAILED) { ok = 0; break; }
      p[0] = 1; p[sz - 1] = 2;
      if (p[0] != 1 || p[sz - 1] != 2) { ok = 0; break; }
      if (munmap(p, sz) != 0) { ok = 0; break; }
    }
    T_ASSERT(ok);
  }

  /* --- 1000 interleaved live mappings --- */
  T_BEGIN("stress/mmap-1000-live");
  {
    void *ptrs[1000];
    int ok = 1, cnt = 0;
    for (int i = 0; i < 1000; i++) {
      ptrs[i] = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                     MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
      if (ptrs[i] == MAP_FAILED) { ok = 0; break; }
      ((char *)ptrs[i])[0] = (char)i;
      cnt++;
    }
    for (int i = 0; i < cnt; i++) {
      if (((char *)ptrs[i])[0] != (char)i) { ok = 0; break; }
      munmap(ptrs[i], 4096);
    }
    T_ASSERT(ok);
  }

  /* --- 64MiB file write + checksum twice, must match --- */
  T_BEGIN("stress/bigfile-checksum");
  {
    const char *path = "t_st_big.bin";
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    T_ASSERT(fd >= 0);
    if (fd >= 0) {
      char buf[65536];
      unsigned x = 12345;
      for (int i = 0; i < (int)sizeof buf; i++) {
        x = x * 1103515245 + 12345;
        buf[i] = (char)(x >> 16);
      }
      int ok = 1;
      for (int i = 0; i < 1024; i++) /* 64 MiB */
        if (write(fd, buf, sizeof buf) != (ssize_t)sizeof buf) { ok = 0; break; }
      close(fd);
      T_ASSERT(ok);
      struct stat st;
      T_ASSERT_OK(stat(path, &st));
      T_ASSERT_EQ(st.st_size, 64LL * 1024 * 1024);
      unsigned long long h1, h2;
      T_ASSERT_OK(checksum(path, &h1));
      T_ASSERT_OK(checksum(path, &h2));
      T_ASSERT(h1 == h2);
      unlink(path);
    }
  }

  return WTEST_END();
}
