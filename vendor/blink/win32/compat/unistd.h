#ifndef WBOX_COMPAT_UNISTD_H
#define WBOX_COMPAT_UNISTD_H
#include <stddef.h>
#include <sys/types.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
#define _SC_PAGESIZE 30
#define _SC_NPROCESSORS_ONLN 84
#define _SC_NPROCESSORS_CONF 83
#define _SC_CLK_TCK 2
#define _SC_PHYS_PAGES 85
#define _SC_AVPHYS_PAGES 86
#define _SC_OPEN_MAX 4
#define _SC_ARG_MAX 0
#define _SC_CHILD_MAX 1
#define _SC_HOST_NAME_MAX 180
#define _SC_TZNAME_MAX 6
#define _SC_VERSION 29
#define _SC_JOB_CONTROL 7
#define _SC_SAVED_IDS 8
#define _PC_NAME_MAX 3
#define _PC_PATH_MAX 4
#define _PC_SYMLINK_MAX 0x100
#define F_OK 0
#define X_OK 1
#define W_OK 2
#define R_OK 4
#define STDIN_FILENO 0
#define STDOUT_FILENO 1
#define STDERR_FILENO 2
#define F_ULOCK 0
#define F_LOCK 1
#define F_TLOCK 2
#define F_TEST 3
ssize_t read(int, void *, size_t);
ssize_t write(int, const void *, size_t);
int close(int);
off_t lseek(int, off_t, int);
int isatty(int);
int dup(int);
int dup2(int, int);
int dup3(int, int, int);
int pipe(int[2]);
int pipe2(int[2], int);
pid_t fork(void);
pid_t vfork(void);
int execve(const char *, char *const *, char *const *);
int execv(const char *, char *const *);
int execvp(const char *, char *const *);
int execlp(const char *, const char *, ...);
int fexecve(int, char *const *, char *const *);
int link(const char *, const char *);
int linkat(int, const char *, int, const char *, int);
int symlink(const char *, const char *);
int symlinkat(const char *, int, const char *);
ssize_t readlink(const char *, char *, size_t);
ssize_t readlinkat(int, const char *, char *, size_t);
int unlink(const char *);
int rmdir(const char *);
int rename(const char *, const char *);
int chdir(const char *);
int fchdir(int);
char *getcwd(char *, size_t);
int access(const char *, int);
int faccessat(int, const char *, int, int);
int fchown(int, uid_t, gid_t);
int chown(const char *, uid_t, gid_t);
int lchown(const char *, uid_t, gid_t);
int fchownat(int, const char *, uid_t, gid_t, int);
int fchmod(int, mode_t);
int fchmodat(int, const char *, mode_t, int);
int chmod(const char *, mode_t);
long fpathconf(int, int);
long pathconf(const char *, int);
uid_t getuid(void);
uid_t geteuid(void);
gid_t getgid(void);
gid_t getegid(void);
int setuid(uid_t);
int setgid(gid_t);
int seteuid(uid_t);
int setegid(gid_t);
int setreuid(uid_t, uid_t);
int setregid(gid_t, gid_t);
int setresuid(uid_t, uid_t, uid_t);
int setresgid(gid_t, gid_t, gid_t);
int getresuid(uid_t *, uid_t *, uid_t *);
int getresgid(gid_t *, gid_t *, gid_t *);
pid_t getpid(void);
pid_t getppid(void);
pid_t getpgrp(void);
pid_t getpgid(pid_t);
int setpgid(pid_t, pid_t);
pid_t setsid(void);
pid_t getsid(pid_t);
pid_t tcgetpgrp(int);
int tcsetpgrp(int, pid_t);
int fsync(int);
int fdatasync(int);
int truncate(const char *, off_t);
int ftruncate(int, off_t);
int lockf(int, int, off_t);
int flock(int, int);
int mkstemp(char *);
int mkfifo(const char *, mode_t);
int mkfifoat(int, const char *, mode_t);
int mknod(const char *, mode_t, dev_t);
int mknodat(int, const char *, mode_t, dev_t);
int nice(int);
void sync(void);
int syncfs(int);
int getdomainname(char *, size_t);
int setdomainname(const char *, size_t);
int sethostname(const char *, size_t);
int gethostname(char *, size_t);
int getdtablesize(void);
int getpagesize(void);
int chroot(const char *);
int daemon(int, int);
int usleep(unsigned);
unsigned sleep(unsigned);
unsigned alarm(unsigned);
int pause(void);
ssize_t pread(int, void *, size_t, off_t);
ssize_t pwrite(int, const void *, size_t, off_t);
int brk(void *);
void *sbrk(intptr_t);
long sysconf(int);
void _exit(int);
int getopt(int, char *const *, const char *);
extern char *optarg;
extern int optind, opterr, optopt;
int sched_yield(void);
#ifdef __cplusplus
}
#endif
#endif
