#ifndef WBOX_COMPAT_SYS_TIME_H
#define WBOX_COMPAT_SYS_TIME_H
#include_next <sys/time.h>
#include <time.h>
#define ITIMER_REAL 0
#define ITIMER_VIRTUAL 1
#define ITIMER_PROF 2
struct itimerval { struct timeval it_interval, it_value; };
int getitimer(int, struct itimerval *);
int setitimer(int, const struct itimerval *, struct itimerval *);
int futimesat(int, const char *, const struct timeval[2]);
int lutimes(const char *, const struct timeval[2]);
int futimes(int, const struct timeval[2]);
#endif
