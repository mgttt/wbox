#ifndef WBOX_COMPAT_SYS_RESOURCE_H
#define WBOX_COMPAT_SYS_RESOURCE_H
#include <sys/types.h>
#include <sys/time.h>
typedef unsigned long long rlim_t;
#define RLIM_INFINITY (~0ULL)
#define RLIM_SAVED_CUR RLIM_INFINITY
#define RLIM_SAVED_MAX RLIM_INFINITY
struct rlimit { rlim_t rlim_cur; rlim_t rlim_max; };
struct rusage {
  struct timeval ru_utime, ru_stime;
  long ru_maxrss, ru_ixrss, ru_idrss, ru_isrss;
  long ru_minflt, ru_majflt, ru_nswap;
  long ru_inblock, ru_oublock, ru_msgsnd, ru_msgrcv;
  long ru_nsignals, ru_nvcsw, ru_nivcsw;
};
#define RUSAGE_SELF 0
#define RUSAGE_CHILDREN -1
#define RLIMIT_CPU 0
#define RLIMIT_FSIZE 1
#define RLIMIT_DATA 2
#define RLIMIT_STACK 3
#define RLIMIT_CORE 4
#define RLIMIT_RSS 5
#define RLIMIT_NPROC 6
#define RLIMIT_NOFILE 7
#define RLIMIT_MEMLOCK 8
#define RLIMIT_AS 9
#define RLIMIT_LOCKS 10
#define RLIMIT_SIGPENDING 11
#define RLIMIT_MSGQUEUE 12
#define RLIMIT_NICE 13
#define RLIMIT_RTPRIO 14
#define RLIMIT_RTTIME 15
#define PRIO_PROCESS 0
#define PRIO_PGRP 1
#define PRIO_USER 2
int getrlimit(int, struct rlimit *);
int setrlimit(int, const struct rlimit *);
int getrusage(int, struct rusage *);
int getpriority(int, int);
int setpriority(int, int, int);
#endif
