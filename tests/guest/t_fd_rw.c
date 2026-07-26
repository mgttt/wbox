/* t_fd_rw.c — pread/pwrite large offsets (>4GB off_t 64-bit regression),
 * readv/writev, truncate/ftruncate. */
#define _GNU_SOURCE
#include <sys/stat.h>
#include <sys/sendfile.h>
#include <sys/uio.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
#include "wtest.h"

int main(void) {
  /* --- pread/pwrite beyond 4GB (off_t must be 64-bit) --- */
  T_BEGIN("pread/pwrite-gt4GB");
  unlink("t_frw_big");
  int fd = open("t_frw_big", O_RDWR | O_CREAT | O_TRUNC, 0600);
  T_ASSERT(fd >= 0);
  off_t far = (off_t)4 * 1024 * 1024 * 1024 + 12345; /* 4GiB + 12345 */
  const char magic[] = "FAR-BEYOND-4G";
  T_ASSERT_EQ(pwrite(fd, magic, sizeof magic, far), sizeof magic);
  struct stat st;
  T_ASSERT_OK(fstat(fd, &st));
  T_ASSERT(st.st_size == (off_t)(far + sizeof magic));
  char rb[sizeof magic];
  T_ASSERT_EQ(pread(fd, rb, sizeof rb, far), sizeof rb);
  T_ASSERT_EQ(memcmp(rb, magic, sizeof magic), 0);
  T_ASSERT_EQ(lseek(fd, far, SEEK_SET), far);
  T_ASSERT_EQ(read(fd, rb, sizeof rb), sizeof rb);
  T_ASSERT_EQ(memcmp(rb, magic, sizeof magic), 0);
  /* hole reads back as zeros */
  char hole[16];
  memset(hole, 0x5A, sizeof hole);
  T_ASSERT_EQ(pread(fd, hole, sizeof hole, far / 2), sizeof hole);
  int z = 1;
  for (size_t i = 0; i < sizeof hole; i++) if (hole[i]) { z = 0; break; }
  T_ASSERT(z);
  /* pread must not move the file position */
  T_ASSERT_EQ(lseek(fd, 0, SEEK_SET), 0);
  char b1;
  T_ASSERT_EQ(pread(fd, rb, sizeof rb, far), sizeof rb);
  T_ASSERT_EQ(read(fd, &b1, 1), 1);
  T_ASSERT(b1 == 0); /* position still 0 -> hole byte */
  T_ASSERT_OK(truncate("t_frw_big", far + 4096));
  T_ASSERT_OK(fstat(fd, &st));
  T_ASSERT(st.st_size == far + 4096);

  /* --- sendfile beyond 4GB, with and without an explicit offset --- */
  T_BEGIN("sendfile/offset-gt4GB");
  {
    char got[sizeof(magic)] = {0};
    int out = open("t_frw_send_dst", O_RDWR | O_CREAT | O_TRUNC, 0600);
    T_ASSERT(out >= 0);
    if (out >= 0) {
      T_ASSERT_EQ(lseek(fd, 7, SEEK_SET), 7);
      off_t pos = far;
      T_ASSERT_EQ(sendfile(out, fd, &pos, sizeof(magic)), sizeof(magic));
      T_ASSERT(pos == far + sizeof(magic));
      T_ASSERT_EQ(lseek(fd, 0, SEEK_CUR), 7);
      T_ASSERT_EQ(pread(out, got, sizeof(got), 0), sizeof(got));
      T_ASSERT_EQ(memcmp(got, magic, sizeof(magic)), 0);

      T_ASSERT_EQ(ftruncate(out, 0), 0);
      T_ASSERT_EQ(lseek(out, 0, SEEK_SET), 0);
      T_ASSERT_EQ(lseek(fd, far, SEEK_SET), far);
      T_ASSERT_EQ(sendfile(out, fd, NULL, sizeof(magic)), sizeof(magic));
      T_ASSERT_EQ(lseek(fd, 0, SEEK_CUR), far + sizeof(magic));
      memset(got, 0, sizeof(got));
      T_ASSERT_EQ(pread(out, got, sizeof(got), 0), sizeof(got));
      T_ASSERT_EQ(memcmp(got, magic, sizeof(magic)), 0);
      T_ASSERT_OK(close(out));
    }
    unlink("t_frw_send_dst");
  }
  T_ASSERT_OK(close(fd));
  unlink("t_frw_big");

  /* --- negative offset / positioned I/O on an empty pipe --- */
  T_BEGIN("pread/negative-ESPIPE-EINVAL");
  {
    int pp[2];
    struct iovec one = {.iov_base = rb, .iov_len = 1};
    T_ASSERT_OK(pipe(pp));
    T_ASSERT_ERRNO(pread(pp[0], rb, 1, 0), ESPIPE);
    T_ASSERT_ERRNO(pwrite(pp[1], "x", 1, 0), ESPIPE);
    T_ASSERT_ERRNO(preadv(pp[0], &one, 1, 0), ESPIPE);
    T_ASSERT_ERRNO(pwritev(pp[1], &one, 1, 0), ESPIPE);
    T_ASSERT_OK(close(pp[0]));
    T_ASSERT_OK(close(pp[1]));
    fd = open("t_frw_x", O_RDWR | O_CREAT | O_TRUNC, 0600);
    T_ASSERT(fd >= 0);
    T_ASSERT_ERRNO(pread(fd, rb, 1, -1), EINVAL);
    close(fd);
    unlink("t_frw_x");
  }

  /* --- readv/writev --- */
  T_BEGIN("readv/writev");
  fd = open("t_frw_v", O_RDWR | O_CREAT | O_TRUNC, 0600);
  T_ASSERT(fd >= 0);
  struct iovec wv[3];
  char a[] = "abc", b[] = "DE", cc[] = "fghi";
  wv[0].iov_base = a; wv[0].iov_len = 3;
  wv[1].iov_base = b; wv[1].iov_len = 2;
  wv[2].iov_base = cc; wv[2].iov_len = 4;
  T_ASSERT_EQ(writev(fd, wv, 3), 9);
  T_ASSERT_EQ(lseek(fd, 0, SEEK_SET), 0);
  char r0[4] = {0}, r1[3] = {0}, r2[5] = {0};
  struct iovec rv[3];
  rv[0].iov_base = r0; rv[0].iov_len = 3;
  rv[1].iov_base = r1; rv[1].iov_len = 2;
  rv[2].iov_base = r2; rv[2].iov_len = 4;
  T_ASSERT_EQ(readv(fd, rv, 3), 9);
  T_ASSERT_EQ(memcmp(r0, "abc", 3), 0);
  T_ASSERT_EQ(memcmp(r1, "DE", 2), 0);
  T_ASSERT_EQ(memcmp(r2, "fghi", 4), 0);
  close(fd);
  unlink("t_frw_v");

  /* --- truncate / ftruncate --- */
  T_BEGIN("truncate/grow-shrink");
  fd = open("t_frw_t", O_RDWR | O_CREAT | O_TRUNC, 0600);
  T_ASSERT(fd >= 0);
  T_ASSERT_EQ(write(fd, "0123456789", 10), 10);
  T_ASSERT_OK(ftruncate(fd, 4));
  T_ASSERT_OK(fstat(fd, &st));
  T_ASSERT_EQ(st.st_size, 4);
  T_ASSERT_OK(ftruncate(fd, 100)); /* grow: tail must be zeros */
  T_ASSERT_OK(fstat(fd, &st));
  T_ASSERT_EQ(st.st_size, 100);
  char tail[6];
  T_ASSERT_EQ(pread(fd, tail, 6, 4), 6);
  int z2 = 1;
  for (int i = 0; i < 6; i++) if (tail[i]) { z2 = 0; break; }
  T_ASSERT(z2);
  close(fd);
  T_ASSERT_OK(truncate("t_frw_t", 2));
  fd = open("t_frw_t", O_RDONLY);
  T_ASSERT_OK(fstat(fd, &st));
  T_ASSERT_EQ(st.st_size, 2);
  close(fd);
  T_ASSERT_ERRNO(truncate("t_frw_t", -5), EINVAL);
  unlink("t_frw_t");

  /* --- lseek SEEK_DATA/HOLE (optional) --- */
  T_BEGIN("lseek/seek-cur-end");
  fd = open("t_frw_s", O_RDWR | O_CREAT | O_TRUNC, 0600);
  T_ASSERT_EQ(write(fd, "0123456789", 10), 10);
  T_ASSERT_EQ(lseek(fd, -3, SEEK_END), 7);
  T_ASSERT_EQ(lseek(fd, 2, SEEK_CUR), 9);
  T_ASSERT_EQ(lseek(fd, 100, SEEK_SET), 100); /* beyond EOF allowed */
  T_ASSERT_ERRNO(lseek(fd, -100, SEEK_SET), EINVAL);
  T_ASSERT_ERRNO(lseek(fd, 0, 9999), EINVAL);
  close(fd);
  unlink("t_frw_s");

  return WTEST_END();
}
