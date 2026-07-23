#ifndef WBOX_COMPAT_SYS_MOUNT_H
#define WBOX_COMPAT_SYS_MOUNT_H
int mount(const char *, const char *, const char *, unsigned long, const void *);
int umount(const char *);
int umount2(const char *, int);
#define MS_RDONLY 1
#define MS_NOSUID 2
#define MS_NODEV 4
#define MS_NOEXEC 8
#define MS_SYNCHRONOUS 16
#define MS_REMOUNT 32
#define MS_MANDLOCK 64
#define MS_DIRSYNC 128
#define MS_NOATIME 1024
#define MS_NODIRATIME 2048
#define MS_BIND 4096
#define MS_MOVE 8192
#define MS_REC 16384
#define MNT_FORCE 1
#endif
