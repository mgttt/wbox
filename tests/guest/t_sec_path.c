/* t_sec_path.c — path-sandbox escape regression (security audit C2).
 *
 * wbox-linux confines guest paths to the rootfs (BLINK_PREFIX). Attempts to
 * escape via intermediate ".." components, absolute paths beyond the rootfs,
 * or symlink chains pointing outside must be denied (EACCES per the audit;
 * ENOENT also acceptable where the audit says the path must not resolve at
 * all — but must never succeed).
 *
 * dirfd anchoring: relative openat() must resolve against the dirfd and must
 * not be escapable with ".." either.
 */
#define _GNU_SOURCE
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
#include "wtest.h"

/* expect open() to FAIL (any errno); success is a sandbox-escape FAIL */
static void deny_open(const char *path) {
  errno = 0;
  int fd = open(path, O_RDONLY);
  int e = errno;
  if (fd == -1) {
    wtest_pass++;
    printf("PASS %s\n", wtest_cur);
  } else {
    wtest_fail++;
    printf("FAIL %s: SANDBOX ESCAPE: open(%s) succeeded (fd=%d)\n",
           wtest_cur, path, fd);
    close(fd);
  }
}

int main(void) {
  /* Probe whether guest is actually confined: /etc/hostname exists on most
   * hosts; if the guest can read a path that provably belongs to the HOST
   * rootfs we can't distinguish. Instead we test relative escape semantics
   * which are unambiguous regardless of rootfs contents. */

  T_BEGIN("sec/leading-dotdot");
  deny_open("/..");
  T_BEGIN("sec/dotdot-past-root");
  deny_open("/../../..");
  T_BEGIN("sec/intermediate-dotdot-escape");
  deny_open("/tmp/../../..");
  T_BEGIN("sec/intermediate-dotdot-file");
  deny_open("/bin/../..../etc/hostname");
  T_BEGIN("sec/many-dotdot");
  deny_open("/a/../../b/../../../c/../../../../etc/passwd");

  /* cwd-relative escape: chdir into a subdir, then open ../.. beyond root */
  T_BEGIN("sec/relative-dotdot-escape");
  {
    T_ASSERT_OK(mkdir("t_sec_sub", 0700));
    int cwdfd = open(".", O_RDONLY | O_DIRECTORY);
    T_ASSERT(cwdfd >= 0);
    T_ASSERT_OK(chdir("t_sec_sub"));
    /* from /<root>/t_sec_sub, ../.. is the parent of the rootfs: must deny */
    deny_open("../../etc/hostname");
    deny_open("../../../..");
    T_ASSERT_OK(fchdir(cwdfd));
    close(cwdfd);
    rmdir("t_sec_sub");
  }

  /* NOTE: absolute host-path probes live in t_sec_path_abshost.c — the
   * current build CRASHES (NULL deref in wbox-linux) on them, which would
   * mask the tests below. */

  /* symlink chain escape: link inside rootfs pointing outside */
  T_BEGIN("sec/symlink-escape");
  {
    unlink("t_sec_link");
    T_ASSERT_OK(symlink("/..", "t_sec_link"));
    deny_open("t_sec_link");
    unlink("t_sec_link");
    T_ASSERT_OK(symlink("/../../etc/hostname", "t_sec_link"));
    deny_open("t_sec_link");
    /* chain: link2 -> link1 -> outside */
    unlink("t_sec_l1"); unlink("t_sec_l2");
    T_ASSERT_OK(symlink("t_sec_link", "t_sec_l1"));
    T_ASSERT_OK(symlink("t_sec_l1", "t_sec_l2"));
    deny_open("t_sec_l2");
    unlink("t_sec_link"); unlink("t_sec_l1"); unlink("t_sec_l2");
  }

  /* dirfd anchoring: openat relative to dirfd stays inside; .. past the
   * dirfd up to root is fine INSIDE the rootfs but past rootfs must deny */
  T_BEGIN("sec/openat-dirfd-anchor");
  {
    T_ASSERT_OK(mkdir("t_sec_d", 0700));
    int fd = open("t_sec_d", O_WRONLY | O_CREAT, 0600);
    if (fd >= 0) close(fd); /* placeholder file t_sec_d can't be both */
    unlink("t_sec_d");
    T_ASSERT_OK(mkdir("t_sec_d", 0700));
    int dfd = open("t_sec_d", O_RDONLY | O_DIRECTORY);
    T_ASSERT(dfd >= 0);
    int f = openat(dfd, "inner.txt", O_WRONLY | O_CREAT | O_TRUNC, 0600);
    T_ASSERT(f >= 0);
    if (f >= 0) { (void)!write(f, "in", 2); close(f); }
    /* verify it landed inside t_sec_d */
    struct stat st;
    T_ASSERT_OK(stat("t_sec_d/inner.txt", &st));
    /* .. from dirfd reaches rootfs root (allowed) */
    f = openat(dfd, "..", O_RDONLY | O_DIRECTORY);
    T_ASSERT(f >= 0);
    if (f >= 0) close(f);
    /* ../.. from dirfd escapes the rootfs: must deny */
    errno = 0;
    f = openat(dfd, "../..", O_RDONLY | O_DIRECTORY);
    if (f == -1) { wtest_pass++; printf("PASS %s\n", wtest_cur); }
    else {
      wtest_fail++;
      printf("FAIL %s: SANDBOX ESCAPE: openat(dirfd, ../..) succeeded\n", wtest_cur);
      close(f);
    }
    close(dfd);
    unlink("t_sec_d/inner.txt");
    rmdir("t_sec_d");
  }

  return WTEST_END();
}
