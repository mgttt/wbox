/*-*- mode:c;indent-tabs-mode:nil;c-basic-offset:2;tab-width:8;coding:utf-8 -*-│
│ vi: set et ft=c ts=2 sts=2 sw=2 fenc=utf-8                               :vi │
╞══════════════════════════════════════════════════════════════════════════════╡
│ Copyright 2023 Trung Nguyen                                                  │
│                                                                              │
│ Permission to use, copy, modify, and/or distribute this software for         │
│ any purpose with or without fee is hereby granted, provided that the         │
│ above copyright notice and this permission notice appear in all copies.      │
│                                                                              │
│ THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL                │
│ WARRANTIES WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED                │
│ WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE             │
│ AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL         │
│ DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR        │
│ PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER               │
│ TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR             │
│ PERFORMANCE OF THIS SOFTWARE.                                                │
╚─────────────────────────────────────────────────────────────────────────────*/
#include "blink/devfs.h"

#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <stdbool.h>
#include <stdio.h>
#include <sys/stat.h>

#include "blink/assert.h"
#include "blink/errno.h"
#include "blink/hostfs.h"
#include "blink/log.h"

#ifndef DISABLE_VFS

#if defined(_WIN32) && !defined(__CYGWIN__)
// wbox win32: the well-known device files map to NUL/CON handles or to
// the fake band fds of w32fd.c (1000000+). Hostfs path resolution would
// look for real files under the (BLINK_PREFIX'ed) host /dev and fail
// with ENOENT/EINVAL, so intercept them here and open the guest-style
// path, which win32's W32OpenSpecial() recognizes.
static int DevfsOpen(struct VfsInfo *parent, const char *name, int flags,
                     int mode, struct VfsInfo **output) {
  struct HostfsInfo *outputinfo;
  struct stat st;
  char gp[16];
  int i;
  if (parent == NULL || name == NULL || output == NULL) {
    return efault();
  }
  if (strcmp(name, "null") && strcmp(name, "zero") && strcmp(name, "full") &&
      strcmp(name, "urandom") && strcmp(name, "random") &&
      strcmp(name, "tty")) {
    return HostfsOpen(parent, name, flags, mode, output);
  }
  if (!S_ISDIR(parent->mode)) {
    return enotdir();
  }
  *output = NULL;
  outputinfo = NULL;
  snprintf(gp, sizeof(gp), "/dev/%s", name);
  if (HostfsCreateInfo(&outputinfo) == -1) {
    return -1;
  }
  outputinfo->filefd = open(gp, flags, mode);  // -> W32OpenSpecial(gp)
  if (getenv("WBOX_DEBUG_FORK"))
    fprintf(stderr, "[devfs] open %s flags=%#x -> %d errno=%d\n", gp, flags,
            outputinfo->filefd, errno);
  if (outputinfo->filefd == -1) {
    goto cleananddie;
  }
  unassert(fstat(outputinfo->filefd, &st) != -1);
  outputinfo->mode = st.st_mode;
  if (VfsCreateInfo(output) == -1) {
    goto cleananddie;
  }
  (*output)->data = outputinfo;
  (*output)->name = strdup(name);
  if ((*output)->name == NULL) {
    enomem();
    goto cleananddie;
  }
  (*output)->namelen = strlen(name);
  unassert(!VfsAcquireDevice(parent->device, &(*output)->device));
  (*output)->dev = parent->dev;
  (*output)->ino = 0x6465762f;  // "dev/" — no host inode behind band fds
  for (i = 0; name[i]; ++i) (*output)->ino = (*output)->ino * 33 + name[i];
  (*output)->mode = outputinfo->mode;
  (*output)->refcount = 1;
  unassert(!VfsAcquireInfo(parent, &(*output)->parent));
  return 0;
cleananddie:
  if (*output) {
    unassert(!VfsFreeInfo(*output));
  } else {
    unassert(!HostfsFreeInfo(outputinfo));
  }
  return -1;
}
static bool DevfsIsW32Device(const char *name) {
  return name && (!strcmp(name, "null") || !strcmp(name, "zero") ||
                  !strcmp(name, "full") || !strcmp(name, "urandom") ||
                  !strcmp(name, "random") || !strcmp(name, "tty"));
}

// Finddir must not touch the host filesystem for our device names: under
// wine the host /dev/null is a real char device and win32 fstatat() on it
// fails (EINVAL), which aborts the whole VfsOpen before Open is reached.
static int DevfsFinddir(struct VfsInfo *parent, const char *name,
                        struct VfsInfo **output) {
  struct HostfsInfo *outputinfo;
  if (!DevfsIsW32Device(name)) {
    return HostfsFinddir(parent, name, output);
  }
  if (parent == NULL || output == NULL) {
    return efault();
  }
  if (!S_ISDIR(parent->mode)) {
    return enotdir();
  }
  *output = NULL;
  outputinfo = NULL;
  if (HostfsCreateInfo(&outputinfo) == -1) {
    return -1;
  }
  outputinfo->mode = S_IFCHR | 0666;
  outputinfo->filefd = -1;
  if (VfsCreateInfo(output) == -1) {
    goto cleananddie;
  }
  (*output)->name = strdup(name);
  if ((*output)->name == NULL) {
    enomem();
    goto cleananddie;
  }
  (*output)->namelen = strlen(name);
  (*output)->data = outputinfo;
  unassert(!VfsAcquireDevice(parent->device, &(*output)->device));
  (*output)->dev = parent->dev;
  (*output)->ino = 0x6465762f;
  for (int i = 0; name[i]; ++i) (*output)->ino = (*output)->ino * 33 + name[i];
  (*output)->mode = outputinfo->mode;
  (*output)->refcount = 1;
  unassert(!VfsAcquireInfo(parent, &(*output)->parent));
  return 0;
cleananddie:
  if (*output) {
    unassert(!VfsFreeInfo(*output));
  } else {
    unassert(!HostfsFreeInfo(outputinfo));
  }
  return -1;
}
#define DEVFS_OPEN DevfsOpen
#define DEVFS_FINDDIR DevfsFinddir
#else
#define DEVFS_OPEN HostfsOpen
#define DEVFS_FINDDIR HostfsFinddir
#endif

static int DevfsInit(const char *source, u64 flags, const void *data,
                     struct VfsDevice **device, struct VfsMount **mount) {
  int ret;
  if (source == NULL) {
    return efault();
  }
  if (*source == '\0') {
    source = "/dev";
  }
  VFS_LOGF("real devfs not implemented, delegating to hostfs");
  if ((ret = HostfsInit(source, flags, data, device, mount)) != -1) {
    (*device)->ops = &g_devfs.ops;
  }
  return ret;
}

static int DevfsReadmountentry(struct VfsDevice *device, char **spec,
                               char **type, char **mntops) {
  *spec = strdup("none");
  if (*spec == NULL) {
    return enomem();
  }
  *type = strdup("devtmpfs");
  if (type == NULL) {
    free(*spec);
    return enomem();
  }
  *mntops = NULL;
  return 0;
}

struct VfsSystem g_devfs = {.name = "devfs",
                            .nodev = true,
                            .ops = {
                                .Init = DevfsInit,
                                .Freeinfo = HostfsFreeInfo,
                                .Freedevice = HostfsFreeDevice,
                                .Finddir = DEVFS_FINDDIR,
                                .Readmountentry = DevfsReadmountentry,
                                .Readlink = HostfsReadlink,
                                .Mkdir = HostfsMkdir,
                                .Mkfifo = HostfsMkfifo,
                                .Open = DEVFS_OPEN,
                                .Access = HostfsAccess,
                                .Stat = HostfsStat,
                                .Fstat = HostfsFstat,
                                .Chmod = HostfsChmod,
                                .Fchmod = HostfsFchmod,
                                .Chown = HostfsChown,
                                .Fchown = HostfsFchown,
                                .Ftruncate = HostfsFtruncate,
                                .Link = HostfsLink,
                                .Unlink = HostfsUnlink,
                                .Read = HostfsRead,
                                .Write = HostfsWrite,
                                .Pread = HostfsPread,
                                .Pwrite = HostfsPwrite,
                                .Readv = HostfsReadv,
                                .Writev = HostfsWritev,
                                .Preadv = HostfsPreadv,
                                .Pwritev = HostfsPwritev,
                                .Seek = HostfsSeek,
                                .Fsync = HostfsFsync,
                                .Fdatasync = HostfsFdatasync,
                                .Flock = HostfsFlock,
                                .Fcntl = HostfsFcntl,
                                .Ioctl = HostfsIoctl,
                                .Dup = HostfsDup,
#ifdef HAVE_DUP3
                                .Dup3 = HostfsDup3,
#endif
                                .Poll = HostfsPoll,
                                .Opendir = HostfsOpendir,
#ifdef HAVE_SEEKDIR
                                .Seekdir = HostfsSeekdir,
                                .Telldir = HostfsTelldir,
#endif
                                .Readdir = HostfsReaddir,
                                .Rewinddir = HostfsRewinddir,
                                .Closedir = HostfsClosedir,
                                .Bind = HostfsBind,
                                .Connect = HostfsConnect,
                                .Connectunix = HostfsConnectUnix,
                                .Accept = HostfsAccept,
                                .Listen = HostfsListen,
                                .Shutdown = HostfsShutdown,
                                .Recvmsg = HostfsRecvmsg,
                                .Sendmsg = HostfsSendmsg,
                                .Recvmsgunix = HostfsRecvmsgUnix,
                                .Sendmsgunix = HostfsSendmsgUnix,
                                .Getsockopt = HostfsGetsockopt,
                                .Setsockopt = HostfsSetsockopt,
                                .Getsockname = HostfsGetsockname,
                                .Getpeername = HostfsGetpeername,
                                .Rename = HostfsRename,
                                .Utime = HostfsUtime,
                                .Futime = HostfsFutime,
                                .Symlink = HostfsSymlink,
                                .Mmap = HostfsMmap,
                                .Munmap = HostfsMunmap,
                                .Mprotect = HostfsMprotect,
                                .Msync = HostfsMsync,
                                .Pipe = NULL,
#ifdef HAVE_PIPE2
                                .Pipe2 = NULL,
#endif
                                .Socket = NULL,
                                .Socketpair = NULL,
                                .Tcgetattr = NULL,
                                .Tcsetattr = NULL,
                                .Tcflush = NULL,
                                .Tcdrain = NULL,
                                .Tcsendbreak = NULL,
                                .Tcflow = NULL,
                                .Tcgetsid = NULL,
                                .Tcgetpgrp = NULL,
                                .Tcsetpgrp = NULL,
#ifdef HAVE_SOCKATMARK
                                .Sockatmark = NULL,
#endif
                                .Fexecve = NULL,
                            }};

#endif /* DISABLE_VFS */
