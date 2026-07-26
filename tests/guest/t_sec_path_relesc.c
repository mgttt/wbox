/* t_sec_path_relesc.c — cwd-relative ".." escape probes (audit C2).
 *
 * ISOLATED: on the current build, open("../../etc/hostname") from a
 * chdir'd subdirectory crashes wbox-linux (wine page fault, NULL deref
 * at wbox-linux+0x5be2b) instead of failing with EACCES. Correct behavior:
 * deny with EACCES — never resolve outside the rootfs, never crash.
 */
#define _GNU_SOURCE
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include "wtest.h"

static void deny_open(const char *path) {
  errno = 0;
  int fd = open(path, O_RDONLY);
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
  T_BEGIN("sec/relesc-setup");
  T_ASSERT_OK(mkdir("t_sre_sub", 0700));
  int cwdfd = open(".", O_RDONLY | O_DIRECTORY);
  T_ASSERT(cwdfd >= 0);
  T_ASSERT_OK(chdir("t_sre_sub"));

  /* from <cwd>/t_sre_sub, "../.." reaches past the cwd root: must deny */
  T_BEGIN("sec/relesc-hostname");
  deny_open("../../etc/hostname");
  T_BEGIN("sec/relesc-parent-of-root");
  deny_open("../../../..");
  T_BEGIN("sec/relesc-deep");
  deny_open("../../../../../../etc/passwd");

  T_BEGIN("sec/relesc-restore");
  T_ASSERT_OK(fchdir(cwdfd));
  close(cwdfd);
  rmdir("t_sre_sub");
  return WTEST_END();
}
