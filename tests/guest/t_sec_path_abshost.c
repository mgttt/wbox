/* t_sec_path_abshost.c — absolute host-path escape probes (audit C2).
 *
 * ISOLATED in its own binary: on the current build, open() of an absolute
 * path that exists only on the HOST crashes wbox-linux with a NULL-pointer
 * deref (wine page fault) instead of failing with EACCES/ENOENT. Keeping
 * these probes separate prevents the crash from masking other tests.
 * Expected correct behavior: open fails (EACCES per audit C2) — never
 * succeeds, never crashes.
 */
#define _GNU_SOURCE
#include <fcntl.h>
#include <unistd.h>
#include "wtest.h"

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
  (void)e;
}

int main(void) {
  /* distinctive host-only paths (this repo / env dir) */
  T_BEGIN("sec/abspath-repo");
  deny_open("/mnt/agents/output/wbox/README.md");
  T_BEGIN("sec/abspath-envdir");
  deny_open("/mnt/agents/wbox-env/setup-tmp.sh");
  T_BEGIN("sec/abspath-host-etc");
  /* /etc/hostname: present on virtually every Linux host; inside the wbox
   * rootfs it normally does NOT exist, so success means host escape */
  deny_open("/etc/hostname");
  return WTEST_END();
}
