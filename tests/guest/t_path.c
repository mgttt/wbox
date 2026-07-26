/* t_path.c — rename/link/symlink/readlink/unlink edge cases,
 * long paths, UTF-8 and special-char filenames. */
#define _GNU_SOURCE
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
#include "wtest.h"

static int write_file(const char *path, const char *s) {
  int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
  if (fd < 0) return -1;
  ssize_t n = write(fd, s, strlen(s));
  close(fd);
  return n == (ssize_t)strlen(s) ? 0 : -1;
}

static int read_file(const char *path, char *buf, size_t cap) {
  int fd = open(path, O_RDONLY);
  if (fd < 0) return -1;
  ssize_t n = read(fd, buf, cap - 1);
  close(fd);
  if (n < 0) return -1;
  buf[n] = 0;
  return (int)n;
}

int main(void) {
  char buf[512];
  struct stat st;

  /* --- rename basics --- */
  T_BEGIN("rename/basic");
  T_ASSERT_OK(write_file("t_p_a", "alpha"));
  T_ASSERT_OK(rename("t_p_a", "t_p_b"));
  T_ASSERT_ERRNO(stat("t_p_a", &st), ENOENT);
  T_ASSERT_EQ(read_file("t_p_b", buf, sizeof buf), 5);
  T_ASSERT_EQ(memcmp(buf, "alpha", 5), 0);

  T_BEGIN("rename/overwrite-existing");
  T_ASSERT_OK(write_file("t_p_c", "charlie"));
  T_ASSERT_OK(rename("t_p_b", "t_p_c")); /* replaces */
  T_ASSERT_EQ(read_file("t_p_c", buf, sizeof buf), 5);
  T_ASSERT_EQ(memcmp(buf, "alpha", 5), 0);

  T_BEGIN("rename/missing-ENOENT");
  T_ASSERT_ERRNO(rename("t_p_nope", "t_p_d"), ENOENT);

  T_BEGIN("rename/dir-across");
  T_ASSERT_OK(mkdir("t_p_d1", 0700));
  T_ASSERT_OK(mkdir("t_p_d2", 0700));
  T_ASSERT_OK(rename("t_p_c", "t_p_d1/inner"));
  T_ASSERT_OK(rename("t_p_d1/inner", "t_p_d2/moved"));
  T_ASSERT_EQ(read_file("t_p_d2/moved", buf, sizeof buf), 5);

  /* --- hard link --- */
  T_BEGIN("link/shared-inode");
  T_ASSERT_OK(write_file("t_p_h", "hardy"));
  T_ASSERT_OK(link("t_p_h", "t_p_h2"));
  struct stat st2;
  T_ASSERT_OK(stat("t_p_h", &st));
  T_ASSERT_OK(stat("t_p_h2", &st2));
  T_ASSERT(st.st_ino == st2.st_ino);
  T_ASSERT_EQ(st.st_nlink, 2);
  T_ASSERT_OK(unlink("t_p_h"));
  T_ASSERT_EQ(read_file("t_p_h2", buf, sizeof buf), 5); /* survives */
  T_ASSERT_OK(stat("t_p_h2", &st));
  T_ASSERT_EQ(st.st_nlink, 1);
  T_ASSERT_ERRNO(link("t_p_nope", "t_p_h3"), ENOENT);

  /* --- symlink --- */
  /* win32 host symlink creation is unavailable (wine 11 only produces
   * inert reparse placeholder files that cannot be followed — verified
   * by probe; wbox reports EPERM). Degrade to SKIP, not FAIL. */
  T_BEGIN("symlink/readlink-follow");
  if (symlink("t_p_h2", "t_p_s") != 0) {
    if (errno == EPERM) {
      T_SKIP("symlink/readlink-follow", "symlink unavailable (win32 host)");
    } else {
      T_ASSERT_OK(symlink("t_p_h2", "t_p_s")); /* FAIL line w/ errno */
    }
  } else {
    ssize_t n = readlink("t_p_s", buf, sizeof buf - 1);
    T_ASSERT(n > 0);
    buf[n] = 0;
    T_ASSERT_EQ(strcmp(buf, "t_p_h2"), 0);
    T_ASSERT_EQ(read_file("t_p_s", buf, sizeof buf), 5); /* follow */
    T_ASSERT_EQ(memcmp(buf, "hardy", 5), 0);
    /* lstat sees the link itself */
    T_ASSERT_OK(lstat("t_p_s", &st));
    T_ASSERT(S_ISLNK(st.st_mode));
    T_ASSERT_OK(stat("t_p_s", &st));
    T_ASSERT(S_ISREG(st.st_mode));
  }

  T_BEGIN("symlink/dangling");
  if (symlink("t_p_gone", "t_p_dang") != 0) {
    if (errno == EPERM) {
      T_SKIP("symlink/dangling", "symlink unavailable (win32 host)");
    } else {
      T_ASSERT_OK(symlink("t_p_gone", "t_p_dang"));
    }
  } else {
    T_ASSERT_ERRNO(open("t_p_dang", O_RDONLY), ENOENT);
    T_ASSERT_OK(lstat("t_p_dang", &st));
    T_ASSERT_OK(unlink("t_p_dang"));
  }

  T_BEGIN("symlink/loop-ELOOP");
  if (symlink("t_p_l2", "t_p_l1") != 0) {
    if (errno == EPERM) {
      T_SKIP("symlink/loop-ELOOP", "symlink unavailable (win32 host)");
    } else {
      T_ASSERT_OK(symlink("t_p_l2", "t_p_l1"));
    }
  } else {
    T_ASSERT_OK(symlink("t_p_l1", "t_p_l2"));
    T_ASSERT_ERRNO(open("t_p_l1", O_RDONLY), ELOOP);
    unlink("t_p_l1"); unlink("t_p_l2");
  }

  /* --- unlink semantics --- */
  T_BEGIN("unlink/open-survives");
  T_ASSERT_OK(write_file("t_p_u", "zz"));
  int fd = open("t_p_u", O_RDONLY);
  T_ASSERT(fd >= 0);
  T_ASSERT_OK(unlink("t_p_u"));
  T_ASSERT_ERRNO(stat("t_p_u", &st), ENOENT);
  T_ASSERT_EQ(read(fd, buf, 2), 2); /* open fd still readable */
  close(fd);
  T_ASSERT_ERRNO(unlink("t_p_u"), ENOENT);
  /* unlink(dir): Linux gives EISDIR, some kernels EPERM; accept either */
  {
    errno = 0;
    int r = unlink("t_p_d2");
    int e = errno;
    if (r == -1 && (e == EISDIR || e == EPERM)) wtest_pass++, printf("PASS %s\n", wtest_cur);
    else { wtest_fail++; printf("FAIL %s: unlink(dir) ret=%d errno=%d\n", wtest_cur, r, e); }
  }

  /* --- long path (nested 20-deep) --- */
  T_BEGIN("path/long-nested");
  {
    char p[512] = {0};
    strcpy(p, "t_p_L");
    T_ASSERT_OK(mkdir(p, 0700));
    int ok = 1;
    for (int i = 0; i < 20; i++) {
      strcat(p, "/1234567890");
      if (mkdir(p, 0700) != 0) { ok = 0; break; }
    }
    T_ASSERT(ok);
    strcat(p, "/leaf.txt");
    T_ASSERT_OK(write_file(p, "deep"));
    T_ASSERT_EQ(read_file(p, buf, sizeof buf), 4);
  }

  /* --- UTF-8 filename --- */
  T_BEGIN("path/utf8-name");
  {
    const char *u = "t_p_\xc3\xa9\xe4\xbd\xa0\xe5\xa5\xbd.txt"; /* é你好 */
    T_ASSERT_OK(write_file(u, "utf8"));
    T_ASSERT_EQ(read_file(u, buf, sizeof buf), 4);
    T_ASSERT_OK(stat(u, &st));
    T_ASSERT_OK(unlink(u));
  }

  /* --- special chars (space, quote, backslash-free) --- */
  T_BEGIN("path/special-chars");
  {
    const char *s = "t_p_ sp ace'quo\"te(br)[k].txt";
    T_ASSERT_OK(write_file(s, "sp"));
    T_ASSERT_EQ(read_file(s, buf, sizeof buf), 2);
    T_ASSERT_OK(unlink(s));
  }

  /* --- dot components in middle --- */
  T_BEGIN("path/dot-dotdot-normalize");
  T_ASSERT_OK(write_file("t_p_d1/x", "xx"));
  T_ASSERT_EQ(read_file("t_p_d1/./x", buf, sizeof buf), 2);
  T_ASSERT_EQ(read_file("t_p_d1/../t_p_d1/x", buf, sizeof buf), 2);

  /* cleanup */
  unlink("t_p_h2"); unlink("t_p_s"); unlink("t_p_d1/inner"); unlink("t_p_d1/x");
  unlink("t_p_d2/moved");
  rmdir("t_p_d1"); rmdir("t_p_d2");
  return WTEST_END();
}
