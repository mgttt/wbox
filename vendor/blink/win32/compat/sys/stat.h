#ifndef WBOX_COMPAT_SYS_STAT_H
#define WBOX_COMPAT_SYS_STAT_H
#include <sys/types.h>
#include <time.h>
#include <fcntl.h>

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

#define S_IFMT 0170000
#define S_IFSOCK 0140000
#define S_IFLNK 0120000
#define S_IFREG 0100000
#define S_IFBLK 0060000
#define S_IFDIR 0040000
#define S_IFCHR 0020000
#define S_IFIFO 0010000
#define S_ISUID 0004000
#define S_ISGID 0002000
#define S_ISVTX 0001000
#define S_IRWXU 0700
#define S_IRUSR 0400
#define S_IWUSR 0200
#define S_IXUSR 0100
#define S_IRWXG 070
#define S_IRGRP 040
#define S_IWGRP 020
#define S_IXGRP 010
#define S_IRWXO 07
#define S_IROTH 04
#define S_IWOTH 02
#define S_IXOTH 01
#define S_ISREG(m) (((m) & S_IFMT) == S_IFREG)
#define S_ISDIR(m) (((m) & S_IFMT) == S_IFDIR)
#define S_ISCHR(m) (((m) & S_IFMT) == S_IFCHR)
#define S_ISBLK(m) (((m) & S_IFMT) == S_IFBLK)
#define S_ISFIFO(m) (((m) & S_IFMT) == S_IFIFO)
#define S_ISLNK(m) (((m) & S_IFMT) == S_IFLNK)
#define S_ISSOCK(m) (((m) & S_IFMT) == S_IFSOCK)
#define S_ISVTX_ 0001000

struct stat {
  dev_t st_dev;
  ino_t st_ino;
  nlink_t st_nlink;
  mode_t st_mode;
  uid_t st_uid;
  gid_t st_gid;
  dev_t st_rdev;
  off_t st_size;
  blksize_t st_blksize;
  blkcnt_t st_blocks;
  struct timespec st_atim;
  struct timespec st_mtim;
  struct timespec st_ctim;
};
#define st_atime st_atim.tv_sec
#define st_mtime st_mtim.tv_sec
#define st_ctime st_ctim.tv_sec

#define major(x) (((unsigned)(x) >> 8) & 0xfff)
#define minor(x) (((unsigned)(x) & 0xff) | (((unsigned)(x) >> 12) & 0xfff00))
#define makedev(maj, min) ((((maj) & 0xfff) << 8) | ((min) & 0xff) | (((min) & 0xfff00) << 12))

int stat(const char *, struct stat *);
int lstat(const char *, struct stat *);
int fstat(int, struct stat *);
int fstatat(int, const char *, struct stat *, int);
int mkdir(const char *, mode_t);
int mkdirat(int, const char *, mode_t);
int mknod(const char *, mode_t, dev_t);
int mknodat(int, const char *, mode_t, dev_t);
int mkfifo(const char *, mode_t);
int mkfifoat(int, const char *, mode_t);
int chmod(const char *, mode_t);
int fchmod(int, mode_t);
mode_t umask(mode_t);
int futimens(int, const struct timespec[2]);
int utimensat(int, const char *, const struct timespec[2], int);
#define UTIME_NOW ((1l << 30) - 1l)
#define UTIME_OMIT ((1l << 30) - 2l)
#endif
