/* t_sec_linkabs.c — symlink with ABSOLUTE target pointing outside rootfs.
 *
 * ISOLATED: on the current build, symlink("/..", ...) crashes wbox-linux
 * (wine page fault, NULL deref) instead of succeeding or failing cleanly.
 * Correct behavior: symlink creation succeeds (it is inert metadata) and
 * the ESCAPE CHECK must happen at open()/stat() time (EACCES per audit C2).
 */
#define _GNU_SOURCE
#include <fcntl.h>
#include <unistd.h>
#include "wtest.h"

int main(void) {
  unlink("t_sla_link");
  T_BEGIN("sec/symlink-abs-target-create");
  if (symlink("/..", "t_sla_link") != 0) {
    /* EPERM on the win32 host is the known host limitation (wine 11
     * cannot create followable symlinks — probe-verified), not a wbox
     * defect: degrade to SKIP. Any other errno is a real FAIL. */
    if (errno != EPERM) {
      T_ASSERT_OK(symlink("/..", "t_sla_link")); /* FAIL line with errno */
    } else {
      wtest_skip++; printf("SKIP %s — %s\n", wtest_cur,
                           "symlink unavailable (win32 host)");
    }
    T_SKIP("sec/symlink-abs-follow", "creation failed");
    return WTEST_END();
  }
  wtest_pass++; printf("PASS %s\n", wtest_cur);

  T_BEGIN("sec/symlink-abs-follow-denied");
  errno = 0;
  int fd = open("t_sla_link", O_RDONLY);
  if (fd == -1) { wtest_pass++; printf("PASS %s\n", wtest_cur); }
  else {
    wtest_fail++;
    printf("FAIL %s: SANDBOX ESCAPE: open(symlink->/..) succeeded\n", wtest_cur);
    close(fd);
  }
  unlink("t_sla_link");
  return WTEST_END();
}
