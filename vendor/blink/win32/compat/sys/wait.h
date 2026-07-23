#ifndef WBOX_COMPAT_SYS_WAIT_H
#define WBOX_COMPAT_SYS_WAIT_H
#include <sys/types.h>
#include <signal.h>
#define WNOHANG 1
#define WUNTRACED 2
#define WCONTINUED 8
#define WEXITSTATUS(s) (((s) & 0xff00) >> 8)
#define WTERMSIG(s) ((s) & 0x7f)
#define WSTOPSIG(s) WEXITSTATUS(s)
#define WIFEXITED(s) (!WTERMSIG(s))
#define WIFSIGNALED(s) (((s) & 0x7f) != 0)
#define WIFSTOPPED(s) (((s) & 0xff) == 0x7f)
#define WIFCONTINUED(s) ((s) == 0xffff)
#define WCOREDUMP(s) ((s) & 0x80)
#define W_STOPCODE(s) ((s) << 8 | 0x7f)
pid_t wait(int *);
pid_t waitpid(pid_t, int *, int);
pid_t wait3(int *, int, struct rusage *);
pid_t wait4(pid_t, int *, int, struct rusage *);
typedef enum { P_ALL, P_PID, P_PGID } idtype_t;
typedef int id_t;
int waitid(idtype_t, id_t, siginfo_t *, int);
#define WEXITED 4
#define WSTOPPED 2
#define WNOWAIT 0x1000000
#endif
