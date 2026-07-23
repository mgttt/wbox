#ifndef WBOX_COMPAT_SYS_SELECT_H
#define WBOX_COMPAT_SYS_SELECT_H
#include <signal.h>
#include <sys/types.h>
#include <signal.h>
#include <sys/time.h>
#include <string.h>
#ifndef FD_SETSIZE
#define FD_SETSIZE 1024
#endif
#ifndef fd_set
typedef struct { unsigned long fds_bits[FD_SETSIZE / (8 * sizeof(unsigned long))]; } fd_set;
#endif
#ifndef FD_ZERO
#define FD_ZERO(s) memset((s), 0, sizeof(fd_set))
#define FD_SET(fd, s) ((s)->fds_bits[(fd) / (8 * sizeof(unsigned long))] |= (1UL << ((fd) % (8 * sizeof(unsigned long)))))
#define FD_CLR(fd, s) ((s)->fds_bits[(fd) / (8 * sizeof(unsigned long))] &= ~(1UL << ((fd) % (8 * sizeof(unsigned long)))))
#define FD_ISSET(fd, s) (!!((s)->fds_bits[(fd) / (8 * sizeof(unsigned long))] & (1UL << ((fd) % (8 * sizeof(unsigned long))))))
#endif
int select(int, fd_set *, fd_set *, fd_set *, struct timeval *);
int pselect(int, fd_set *, fd_set *, fd_set *, const struct timespec *, const sigset_t *);
#endif
