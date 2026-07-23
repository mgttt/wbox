#ifndef WBOX_COMPAT_SYS_TIMES_H
#define WBOX_COMPAT_SYS_TIMES_H
#include <sys/types.h>
struct tms { clock_t tms_utime, tms_stime, tms_cutime, tms_cstime; };
clock_t times(struct tms *);
#endif
