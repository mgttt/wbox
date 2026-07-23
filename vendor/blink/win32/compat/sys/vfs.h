#ifndef WBOX_COMPAT_SYS_VFS_H
#define WBOX_COMPAT_SYS_VFS_H
#include <sys/statvfs.h>
#define statfs statvfs
#define fstatfs fstatvfs
#endif
