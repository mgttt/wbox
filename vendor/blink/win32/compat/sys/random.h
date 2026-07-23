#ifndef WBOX_COMPAT_SYS_RANDOM_H
#define WBOX_COMPAT_SYS_RANDOM_H
#include <sys/types.h>
ssize_t getrandom(void *, size_t, unsigned int);
#define GRND_NONBLOCK 1
#define GRND_RANDOM 2
#endif
