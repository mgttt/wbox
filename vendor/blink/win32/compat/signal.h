#ifndef WBOX_COMPAT_SIGNAL_H
#define WBOX_COMPAT_SIGNAL_H
#include_next <signal.h>
#include <stdint.h>
#undef pthread_sigmask
#undef pthread_kill
#include <sys/types.h>
#ifndef WBOX_SIGSET_T
#define WBOX_SIGSET_T
typedef struct { unsigned long __bits[2]; } sigset_t;
#endif
#define SIGHUP 1
#define SIGQUIT 3
#define SIGTRAP 5
#define SIGABRT 6
#define SIGBUS 7
#define SIGKILL 9
#define SIGUSR1 10
#define SIGSEGV 11
#define SIGUSR2 12
#define SIGPIPE 13
#define SIGALRM 14
#define SIGSTKFLT 16
#define SIGCHLD 17
#define SIGCONT 18
#define SIGSTOP 19
#define SIGTSTP 20
#define SIGTTIN 21
#define SIGTTOU 22
#define SIGURG 23
#define SIGXCPU 24
#define SIGXFSZ 25
#define SIGVTALRM 26
#define SIGPROF 27
#define SIGWINCH 28
#define SIGIO 29
#define SIGPWR 30
#define SIGSYS 31
#define SIGCLD SIGCHLD
#define SIGPOLL SIGIO
#define NSIG 65
#define SIG_BLOCK 0
#define SIG_UNBLOCK 1
#define SIG_SETMASK 2
#define SA_NOCLDSTOP 1
#define SA_NOCLDWAIT 2
#define SA_SIGINFO 4
#define SA_ONSTACK 0x08000000
#define SA_RESTART 0x10000000
#define SA_NODEFER 0x40000000
#define SA_RESETHAND 0x80000000
#define SA_RESTORER 0x04000000
#define SS_ONSTACK 1
#define SS_DISABLE 2
#define MINSIGSTKSZ 2048
#define SIGSTKSZ 8192
#define SI_USER 0
#define SI_KERNEL 0x80
#define SI_QUEUE -1
#define SI_TIMER -2
#define SI_ASYNCIO -4
#define SI_TKILL -6
#define SEGV_MAPERR 1
#define SEGV_ACCERR 2
#define BUS_ADRALN 1
#define ILL_ILLOPC 1
#define ILL_ILLOPN 2
#define ILL_ILLADR 3
#define ILL_ILLTRP 4
#define ILL_PRVOPC 5
#define ILL_PRVREG 6
#define ILL_COPROC 7
#define ILL_BADSTK 8
#define FPE_INTDIV 1
#define FPE_INTOVF 2
#define FPE_FLTDIV 3
#define FPE_FLTOVF 4
#define FPE_FLTUND 5
#define FPE_FLTRES 6
#define FPE_FLTINV 7
#define FPE_FLTSUB 8
#define TRAP_BRKPT 1
#define TRAP_TRACE 2
#define CLD_EXITED 1
#define CLD_KILLED 2
#define CLD_DUMPED 3
#define CLD_TRAPPED 4
#define CLD_STOPPED 5
#define CLD_CONTINUED 6
#define POLL_IN 1
#define POLL_OUT 2
#define POLL_MSG 3
#define POLL_ERR 4
#define POLL_PRI 5
#define POLL_HUP 6
typedef struct siginfo {
  int si_signo, si_errno, si_code;
  pid_t si_pid;
  uid_t si_uid;
  void *si_addr;
  int si_status;
  long si_band;
  union { int __pad[20]; } __si_fields;
} siginfo_t;
typedef void (*sa_sigaction_t)(int, siginfo_t *, void *);
struct sigaction {
  union {
    void (*sa_handler)(int);
    sa_sigaction_t sa_sigaction;
  } __u;
  sigset_t sa_mask;
  int sa_flags;
  void (*sa_restorer)(void);
};
#define sa_handler __u.sa_handler
#define sa_sigaction __u.sa_sigaction
typedef struct __wbox_stack {
  void *ss_sp;
  int ss_flags;
  size_t ss_size;
} stack_t;
#define SIGEV_SIGNAL 0
#define SIGEV_NONE 1
#define SIGEV_THREAD 2
#define SIGEV_THREAD_ID 4
union sigval { int sival_int; void *sival_ptr; };
struct sigevent {
  union sigval sigev_value;
  int sigev_signo;
  int sigev_notify;
  int __pad[8];
};
int sigaction(int, const struct sigaction *, struct sigaction *);
int sigprocmask(int, const sigset_t *, sigset_t *);
int sigemptyset(sigset_t *);
int sigfillset(sigset_t *);
int sigaddset(sigset_t *, int);
int sigdelset(sigset_t *, int);
int sigismember(const sigset_t *, int);
int sigsuspend(const sigset_t *);
int sigpending(sigset_t *);
int sigaltstack(const stack_t *, stack_t *);
int kill(pid_t, int);
int killpg(pid_t, int);
int sigqueue(pid_t, int, const union sigval);
int sighold(int);
int sigrelse(int);
int sigignore(int);
int pthread_sigmask(int, const sigset_t *, sigset_t *);
int pthread_kill(uintptr_t, int);
#include <setjmp.h>
typedef jmp_buf sigjmp_buf;
#define sigsetjmp(env, save) setjmp(env)
#define siglongjmp(env, val) longjmp(env, val)
char *strsignal(int);
#endif
