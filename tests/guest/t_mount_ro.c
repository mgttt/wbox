#define _GNU_SOURCE
#include "wtest.h"

#include <errno.h>
#include <fcntl.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <unistd.h>

static void expect_ro_failure(const char *name, int rc) {
  T_BEGIN(name);
  T_ASSERT(rc == -1 && errno == EROFS);
}

int main(void) {
  int fd;
  char buf[8] = {0};

  mkdir("ro-src", 0755);
  mkdir("ro-mnt", 0755);
  fd = open("ro-src/data", O_CREAT | O_WRONLY | O_TRUNC, 0644);
  T_BEGIN("fixture/open");
  T_ASSERT(fd >= 0);
  T_BEGIN("fixture/write");
  T_ASSERT(write(fd, "seed", 4) == 4);
  close(fd);

  T_BEGIN("mount/readonly");
  T_ASSERT(mount("ro-src", "ro-mnt", "hostfs", MS_RDONLY, NULL) == 0);

  fd = open("ro-mnt/data", O_RDONLY);
  T_BEGIN("readonly/read-open");
  T_ASSERT(fd >= 0);
  T_BEGIN("readonly/read");
  T_ASSERT(read(fd, buf, sizeof(buf)) == 4 && !memcmp(buf, "seed", 4));
  close(fd);

  errno = 0;
  expect_ro_failure("readonly/open-write", open("ro-mnt/data", O_WRONLY));
  errno = 0;
  expect_ro_failure("readonly/create",
                    open("ro-mnt/new", O_CREAT | O_WRONLY, 0644));
  errno = 0;
  expect_ro_failure("readonly/truncate", truncate("ro-mnt/data", 0));
  errno = 0;
  expect_ro_failure("readonly/unlink", unlink("ro-mnt/data"));
  errno = 0;
  expect_ro_failure("readonly/mkdir", mkdir("ro-mnt/newdir", 0755));
  errno = 0;
  expect_ro_failure("readonly/chmod", chmod("ro-mnt/data", 0600));
  errno = 0;
  expect_ro_failure("readonly/rename",
                    rename("ro-mnt/data", "ro-mnt/moved"));
  errno = 0;
  expect_ro_failure("readonly/link",
                    link("ro-mnt/data", "ro-mnt/linked"));

  return WTEST_END();
}
