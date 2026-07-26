/* t_fd_open.c — open flag matrix: O_RDWR/O_APPEND/O_TRUNC/O_CREAT mode bits,
 * O_TMPFILE, plus dup family semantics. */
#define _GNU_SOURCE
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
#include "wtest.h"

int main(void) {
  char buf[64];

  /* --- O_CREAT mode bits --- */
  T_BEGIN("open/ocreat-mode");
  unlink("t_fo_a");
  int fd = open("t_fo_a", O_RDWR | O_CREAT | O_EXCL, 0604);
  T_ASSERT(fd >= 0);
  struct stat st;
  T_ASSERT_OK(fstat(fd, &st));
  T_ASSERT_EQ(st.st_mode & 0777, 0604);
  close(fd);
  /* O_EXCL on existing must fail */
  T_ASSERT_ERRNO(open("t_fo_a", O_RDWR | O_CREAT | O_EXCL, 0600), EEXIST);

  /* --- O_RDWR read+write --- */
  T_BEGIN("open/ordwr-rw");
  fd = open("t_fo_a", O_RDWR);
  T_ASSERT(fd >= 0);
  T_ASSERT_EQ(write(fd, "hello", 5), 5);
  T_ASSERT_EQ(lseek(fd, 0, SEEK_SET), 0);
  memset(buf, 0, sizeof buf);
  T_ASSERT_EQ(read(fd, buf, 5), 5);
  T_ASSERT_EQ(memcmp(buf, "hello", 5), 0);
  close(fd);

  /* --- O_APPEND --- */
  T_BEGIN("open/oappend-forces-eof");
  fd = open("t_fo_a", O_WRONLY | O_APPEND);
  T_ASSERT(fd >= 0);
  T_ASSERT_EQ(lseek(fd, 0, SEEK_SET), 0); /* try to rewind */
  T_ASSERT_EQ(write(fd, "XY", 2), 2);     /* must land at EOF anyway */
  close(fd);
  fd = open("t_fo_a", O_RDONLY);
  memset(buf, 0, sizeof buf);
  T_ASSERT_EQ(read(fd, buf, sizeof buf), 7);
  T_ASSERT_EQ(memcmp(buf, "helloXY", 7), 0);
  close(fd);
  /* pwrite under O_APPEND still appends (Linux semantics) */
  T_BEGIN("open/oappend-pwrite-appends");
  fd = open("t_fo_a", O_WRONLY | O_APPEND);
  T_ASSERT_EQ(pwrite(fd, "Z", 1, 0), 1);
  close(fd);
  fd = open("t_fo_a", O_RDONLY);
  memset(buf, 0, sizeof buf);
  T_ASSERT_EQ(read(fd, buf, sizeof buf), 8);
  T_ASSERT_EQ(memcmp(buf, "helloXYZ", 8), 0);
  close(fd);

  /* --- O_TRUNC --- */
  T_BEGIN("open/otrunc");
  fd = open("t_fo_a", O_WRONLY | O_TRUNC);
  T_ASSERT(fd >= 0);
  T_ASSERT_OK(fstat(fd, &st));
  T_ASSERT_EQ(st.st_size, 0);
  close(fd);

  /* --- O_TMPFILE --- */
  T_BEGIN("open/otmpfile");
  fd = open(".", O_RDWR | O_TMPFILE, 0600);
  if (fd == -1 && (errno == EOPNOTSUPP || errno == EISDIR || errno == ENOENT)) {
    T_SKIP("open/otmpfile", "O_TMPFILE unsupported");
  } else {
    T_ASSERT(fd >= 0);
    T_ASSERT_EQ(write(fd, "tmpdata", 7), 7);
    T_ASSERT_EQ(lseek(fd, 0, SEEK_SET), 0);
    memset(buf, 0, sizeof buf);
    T_ASSERT_EQ(read(fd, buf, sizeof buf), 7);
    T_ASSERT_EQ(memcmp(buf, "tmpdata", 7), 0);
    close(fd);
  }

  /* --- dup / dup2 / dup3 / F_DUPFD --- */
  T_BEGIN("dup/shared-offset");
  fd = open("t_fo_b", O_RDWR | O_CREAT | O_TRUNC, 0600);
  T_ASSERT(fd >= 0);
  T_ASSERT_EQ(write(fd, "0123456789", 10), 10);
  int d2 = dup(fd);
  T_ASSERT(d2 >= 0 && d2 != fd);
  T_ASSERT_EQ(lseek(fd, 5, SEEK_SET), 5);
  char c;
  T_ASSERT_EQ(read(d2, &c, 1), 1); /* dup shares file offset */
  T_ASSERT(c == '5');

  T_BEGIN("dup2/replaces-target");
  int d3 = open("/dev/null", O_RDONLY);
  T_ASSERT(d3 >= 0);
  T_ASSERT_EQ(dup2(fd, d3), d3);
  T_ASSERT_EQ(read(d3, &c, 1), 1); /* continues at offset 6 */
  T_ASSERT(c == '6');
  close(d3);

  T_BEGIN("dup3/cloexec");
  errno = 0;
  int d4 = dup3(fd, fd, 0);
  T_ASSERT(d4 == -1 && errno == EINVAL); /* same fd must fail */
  d4 = dup3(fd, 42, O_CLOEXEC);
  if (d4 == -1 && errno == ENOSYS) {
    T_SKIP("dup3/cloexec", "dup3 unimplemented");
  } else {
    T_ASSERT(d4 == 42);
    int fl = fcntl(42, F_GETFD);
    T_ASSERT(fl >= 0 && (fl & FD_CLOEXEC));
    close(42);
  }

  T_BEGIN("fcntl/F_DUPFD-min");
  int d5 = fcntl(fd, F_DUPFD, 10);
  T_ASSERT(d5 >= 10);
  T_ASSERT_EQ(lseek(d5, 0, SEEK_CUR), 7); /* shared offset at 7 */
  close(d5);

  T_BEGIN("fcntl/F_DUPFD_CLOEXEC");
  int d6 = fcntl(fd, F_DUPFD_CLOEXEC, 20);
  if (d6 == -1 && errno == EINVAL) {
    T_SKIP("fcntl/F_DUPFD_CLOEXEC", "unimplemented");
  } else {
    T_ASSERT(d6 >= 20);
    int fl = fcntl(d6, F_GETFD);
    T_ASSERT(fl >= 0 && (fl & FD_CLOEXEC));
    close(d6);
  }
  close(fd);

  T_BEGIN("ioctl/cloexec-toggle");
  fd = open("/dev/null", O_RDONLY);
  T_ASSERT(fd >= 0);
  T_ASSERT_OK(ioctl(fd, FIOCLEX));
  T_ASSERT(fcntl(fd, F_GETFD) & FD_CLOEXEC);
  T_ASSERT_OK(ioctl(fd, FIONCLEX));
  T_ASSERT(!(fcntl(fd, F_GETFD) & FD_CLOEXEC));
  close(fd);

  T_BEGIN("dup/shared-append-status");
  {
    char got[5] = {0};
    fd = open("t_fo_append_dup", O_RDWR | O_CREAT | O_TRUNC, 0600);
    T_ASSERT(fd >= 0);
    T_ASSERT_EQ(write(fd, "abc", 3), 3);
    d2 = dup(fd);
    T_ASSERT(d2 >= 0);
    T_ASSERT_OK(fcntl(d2, F_SETFL, O_APPEND));
    T_ASSERT(fcntl(fd, F_GETFL) & O_APPEND);
    T_ASSERT_EQ(lseek(fd, 0, SEEK_SET), 0);
    T_ASSERT_EQ(write(fd, "X", 1), 1);
    T_ASSERT_EQ(pread(d2, got, 4, 0), 4);
    T_ASSERT_EQ(memcmp(got, "abcX", 4), 0);
    T_ASSERT_OK(fcntl(fd, F_SETFL, 0));
    T_ASSERT(!(fcntl(d2, F_GETFL) & O_APPEND));
    close(d2);
    close(fd);
    unlink("t_fo_append_dup");
  }

  /* close original must not invalidate dup'd description's open file */
  T_BEGIN("dup/close-original-survives");
  fd = open("t_fo_b", O_RDONLY);
  d2 = dup(fd);
  close(fd);
  T_ASSERT_EQ(read(d2, &c, 1), 1);
  T_ASSERT(c == '0');
  close(d2);

  T_BEGIN("pipe/fork-child-lowest-fds");
  {
    pid_t pid = fork();
    T_ASSERT(pid >= 0);
    if (pid == 0) {
      int p[2] = {-1, -1};
      close(STDIN_FILENO);
      if (pipe(p) != 0) _exit(1);
      if (p[0] != STDIN_FILENO) _exit(2);
      if (p[1] != 3) _exit(3);
      _exit(0);
    }
    if (pid > 0) {
      int status = 0;
      T_ASSERT_EQ(waitpid(pid, &status, 0), pid);
      T_ASSERT(WIFEXITED(status));
      T_ASSERT_EQ(WEXITSTATUS(status), 0);
    }
  }

  unlink("t_fo_a");
  unlink("t_fo_b");
  return WTEST_END();
}
