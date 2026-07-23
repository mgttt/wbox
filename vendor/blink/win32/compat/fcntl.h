#ifndef WBOX_COMPAT_FCNTL_H
#define WBOX_COMPAT_FCNTL_H
#include <sys/types.h>
#include <sys/types.h>
#ifndef O_ACCMODE
#define O_ACCMODE 3
#endif
#ifndef O_RDONLY
#define O_RDONLY 0
#define O_WRONLY 1
#define O_RDWR 2
#endif
#undef O_CREAT
#undef O_EXCL
#undef O_TRUNC
#undef O_APPEND
#undef O_BINARY
#undef O_TEXT
#undef O_NOINHERIT
#undef O_TEMPORARY
#undef O_SHORT_LIVED
#undef O_SEQUENTIAL
#undef O_RANDOM
#define O_CREAT 0x40
#define O_EXCL 0x80
#define O_NOCTTY 0x100
#define O_TRUNC 0x200
#define O_APPEND 0x400
#define O_NONBLOCK 0x800
#define O_NDELAY O_NONBLOCK
#define O_SYNC 0x101000
#define O_DSYNC 0x1000
#define O_DIRECT 0x4000
#define O_LARGEFILE 0x8000
#define O_DIRECTORY 0x10000
#define O_NOFOLLOW 0x20000
#define O_NOATIME 0x40000
#define O_CLOEXEC 0x80000
#define O_PATH 0x200000
#define O_TMPFILE 0x410000
#define O_RSYNC O_SYNC
#define F_GETFL 3
#define F_SETFL 4
#define F_GETFD 1
#define F_SETFD 2
#define F_GETLK 5
#define F_SETLK 6
#define F_SETLKW 7
#define F_DUPFD 0
#define F_DUPFD_CLOEXEC 1030
#define F_GETOWN 9
#define F_SETOWN 8
#define F_GETOWN_EX 16
#define F_SETOWN_EX 15
#define F_GETSIG 11
#define F_SETSIG 10
#define F_SETPIPE_SZ 1031
#define F_GETPIPE_SZ 1032
#define F_NOTIFY 1026
#define F_RDLCK 0
#define F_WRLCK 1
#define F_UNLCK 2
#define FD_CLOEXEC 1
#define AT_FDCWD -100
#define AT_SYMLINK_NOFOLLOW 0x100
#define AT_REMOVEDIR 0x200
#define AT_SYMLINK_FOLLOW 0x400
#define AT_NO_AUTOMOUNT 0x800
#define AT_EMPTY_PATH 0x1000
#define AT_EACCESS 0x200
struct flock { short l_type, l_whence; off_t l_start, l_len; pid_t l_pid; };
int fcntl(int, int, ...);
int openat(int, const char *, int, ...);
int open(const char *, int, ...);
int creat(const char *, mode_t);
#define LOCK_SH 1
#define LOCK_EX 2
#define LOCK_NB 4
#define LOCK_UN 8
#define DN_ACCESS 1
#define DN_MODIFY 2
#define DN_CREATE 4
#define DN_DELETE 8
#define DN_RENAME 16
#define DN_ATTRIB 32
#define DN_MULTISHOT 0x80000000
#endif
