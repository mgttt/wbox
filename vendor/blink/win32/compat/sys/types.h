#ifndef WBOX_COMPAT_SYS_TYPES_H
#define WBOX_COMPAT_SYS_TYPES_H
#include_next <sys/types.h>
#ifndef WBOX_UID_T
#define WBOX_UID_T
typedef unsigned int uid_t;
typedef unsigned int gid_t;
#endif
#ifndef WBOX_NLINK_T
#define WBOX_NLINK_T
typedef unsigned int nlink_t;
#endif
#ifndef WBOX_BLKSIZE_T
#define WBOX_BLKSIZE_T
typedef long blksize_t;
#endif
#ifndef WBOX_BLKCNT_T
#define WBOX_BLKCNT_T
typedef long long blkcnt_t;
#endif
#ifndef WBOX_FSBLKCNT_T
#define WBOX_FSBLKCNT_T
typedef unsigned long long fsblkcnt_t;
typedef unsigned long long fsfilcnt_t;
#endif
#ifndef WBOX_CLOCK_T
#define WBOX_CLOCK_T
typedef long suseconds_t_;
#endif
typedef unsigned long u_long;
typedef unsigned int u_int;
typedef unsigned short u_short;
typedef unsigned char u_char;
#endif
