/* t_negative.c — errno precision for invalid fds, bad pointers, oversized
 * args, illegal flag combos. Crash-prone probes run in forked children so a
 * guest SIGSEGV surfaces as FAIL instead of aborting the whole suite. */
#define _GNU_SOURCE
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <sys/uio.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
#include <limits.h>
#include "wtest.h"

/* run fn in a child; expect it to _exit(0). Any signal/other exit = FAIL */
static void in_child(int (*fn)(void)) {
  pid_t pid = fork();
  if (pid == 0) _exit(fn() == 0 ? 0 : 1);
  int st;
  if (pid < 0 || waitpid(pid, &st, 0) != pid || !WIFEXITED(st) || WEXITSTATUS(st) != 0) {
    wtest_fail++;
    printf("FAIL %s: child abnormal (pid=%d status=0x%x)\n",
           wtest_cur, (int)pid, pid > 0 ? st : -1);
  } else {
    wtest_pass++;
    printf("PASS %s\n", wtest_cur);
  }
}

static int chk_badbuf_read(void) {
  /* read into an unmapped address must EFAULT (or crash -> caught by parent) */
  int pp[2];
  if (pipe(pp)) return 1;
  if (write(pp[1], "x", 1) != 1) return 1;
  errno = 0;
  ssize_t r = read(pp[0], (void *)0x1000, 1); /* page 0x1000: unmapped */
  int ok = (r == -1 && errno == EFAULT);
  close(pp[0]); close(pp[1]);
  return ok ? 0 : 1;
}

static int chk_badbuf_write(void) {
  int pp[2];
  if (pipe(pp)) return 1;
  errno = 0;
  ssize_t r = write(pp[1], (const void *)0x1000, 1);
  int ok = (r == -1 && errno == EFAULT);
  close(pp[0]); close(pp[1]);
  return ok ? 0 : 1;
}

static int chk_prot_none_read(void) {
  char *p = mmap(NULL, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (p == MAP_FAILED) return 1;
  int pp[2];
  if (pipe(pp)) return 1;
  if (write(pp[1], "x", 1) != 1) return 1;
  errno = 0;
  ssize_t r = read(pp[0], p, 1); /* PROT_NONE page */
  int ok = (r == -1 && errno == EFAULT);
  close(pp[0]); close(pp[1]);
  munmap(p, 4096);
  return ok ? 0 : 1;
}

int main(void) {
  char b[16];
  struct stat st;

  /* --- invalid fd --- */
  T_BEGIN("neg/badfd-ebadf");
  T_ASSERT_ERRNO(read(9999, b, 1), EBADF);
  T_ASSERT_ERRNO(write(9999, b, 1), EBADF);
  T_ASSERT_ERRNO(close(9999), EBADF);
  T_ASSERT_ERRNO(lseek(9999, 0, SEEK_SET), EBADF);
  T_ASSERT_ERRNO(fstat(9999, &st), EBADF);
  T_ASSERT_ERRNO(dup(9999), EBADF);
  T_ASSERT_ERRNO(fcntl(9999, F_GETFL), EBADF);
  T_ASSERT_ERRNO(fcntl(9999, F_DUPFD, -1), EBADF);

  T_BEGIN("neg/closed-fd-ebadf");
  {
    int fd = open("/dev/null", O_RDONLY);
    T_ASSERT(fd >= 0);
    close(fd);
    T_ASSERT_ERRNO(read(fd, b, 1), EBADF);
  }

  T_BEGIN("neg/dupfd-min-range");
  {
    int fd = open("/dev/null", O_RDONLY);
    T_ASSERT(fd >= 0);
    T_ASSERT_ERRNO(fcntl(fd, F_DUPFD, -1), EINVAL);
    T_ASSERT_ERRNO(fcntl(fd, F_DUPFD, INT_MAX), EINVAL);
    T_ASSERT_ERRNO(fcntl(fd, F_DUPFD_CLOEXEC, -1), EINVAL);
    T_ASSERT_ERRNO(fcntl(fd, F_DUPFD_CLOEXEC, INT_MAX), EINVAL);
    close(fd);
  }

  /* --- fd used against its type --- */
  T_BEGIN("neg/dirfd-read-EISDIR");
  {
    int fd = open(".", O_RDONLY | O_DIRECTORY);
    T_ASSERT(fd >= 0);
    T_ASSERT_ERRNO(read(fd, b, 1), EISDIR);
    close(fd);
  }

  T_BEGIN("neg/write-rdonly-EBADF");
  {
    T_ASSERT_ERRNO(write(STDIN_FILENO, "x", 1), EBADF);
  }

  /* --- bad pointers (in children; a SIGSEGV = FAIL, not suite death) --- */
  T_BEGIN("neg/efault-read-badbuf");
  in_child(chk_badbuf_read);
  T_BEGIN("neg/efault-write-badbuf");
  in_child(chk_badbuf_write);
  T_BEGIN("neg/efault-read-prot-none");
  in_child(chk_prot_none_read);

  /* --- syscall arg edges --- */
  T_BEGIN("neg/unknown-open-flag");
  {
    /* kernels differ: must either reject EINVAL or ignore; anything else odd */
    errno = 0;
    int fd = open("t_neg_f", O_RDONLY | O_CREAT | 0x40000000, 0600);
    if (fd == -1 && errno == EINVAL) {
      wtest_pass++; printf("PASS %s\n", wtest_cur);
    } else if (fd >= 0) {
      wtest_skip++; printf("SKIP %s — unknown flag tolerated\n", wtest_cur);
      close(fd); unlink("t_neg_f");
    } else {
      wtest_fail++; printf("FAIL %s: ret=%d errno=%d\n", wtest_cur, fd, errno);
    }
  }
  T_ASSERT_ERRNO(open("", O_RDONLY), ENOENT);
  {
    char longname[5000];
    memset(longname, 'a', sizeof longname - 1);
    longname[sizeof longname - 1] = 0;
    T_ASSERT_ERRNO(open(longname, O_RDONLY), ENAMETOOLONG);
  }
  T_ASSERT_ERRNO(readv(9999, NULL, 0), EBADF);
  {
    struct iovec iov = {b, 1};
    T_ASSERT_ERRNO(readv(0, &iov, -1), EINVAL);
  }

  /* --- socket misuse --- */
  T_BEGIN("neg/socket-bad-family");
  T_ASSERT_ERRNO(socket(9999, SOCK_STREAM, 0), EAFNOSUPPORT);
  T_BEGIN("neg/socket-bad-type");
  {
    errno = 0;
    int fd = socket(AF_INET, 9999, 0);
    int e = errno;
    if (fd == -1 && (e == ESOCKTNOSUPPORT || e == EPROTONOSUPPORT || e == EINVAL)) {
      wtest_pass++; printf("PASS %s\n", wtest_cur);
    } else {
      wtest_fail++; printf("FAIL %s: socket(AF_INET,9999) ret=%d errno=%d\n", wtest_cur, fd, e);
    }
  }

  /* --- waitpid edges --- */
  T_BEGIN("neg/waitpid-echild");
  T_ASSERT_ERRNO(waitpid(999999, NULL, 0), ECHILD);

  /* --- kill self-invalid --- */
  T_BEGIN("neg/kill-bad-sig");
  T_ASSERT_ERRNO(kill(getpid(), 9999), EINVAL);

  return WTEST_END();
}
