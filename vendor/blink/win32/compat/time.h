#ifndef WBOX_COMPAT_TIME_H
#define WBOX_COMPAT_TIME_H
#include_next <time.h>
struct tm *gmtime_r(const time_t *, struct tm *);
struct tm *localtime_r(const time_t *, struct tm *);
char *asctime_r(const struct tm *, char *);
char *ctime_r(const time_t *, char *);
time_t timegm(struct tm *);
int clock_gettime(int, struct timespec *);
int clock_settime(int, const struct timespec *);
int clock_nanosleep(int, int, const struct timespec *, struct timespec *);
int nanosleep(const struct timespec *, struct timespec *);
#define CLOCK_REALTIME 0
#define CLOCK_MONOTONIC 1
#define CLOCK_PROCESS_CPUTIME_ID 2
#define CLOCK_THREAD_CPUTIME_ID 3
#define CLOCK_MONOTONIC_RAW 4
#define CLOCK_REALTIME_COARSE 5
#define CLOCK_MONOTONIC_COARSE 6
#define CLOCK_BOOTTIME 7
#define TIMER_ABSTIME 1
typedef int timer_t;

int timer_create(int, struct sigevent *, timer_t *);
int timer_settime(timer_t, int, const struct itimerspec *, struct itimerspec *);
int timer_gettime(timer_t, struct itimerspec *);
int timer_delete(timer_t);
char *strptime(const char *, const char *, struct tm *);
int timespec_get(struct timespec *, int);
#endif
