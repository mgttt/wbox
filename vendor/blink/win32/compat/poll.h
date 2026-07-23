#ifndef WBOX_COMPAT_POLL_H
#define WBOX_COMPAT_POLL_H
#include <signal.h>
typedef unsigned long nfds_t;
struct pollfd { int fd; short events; short revents; };
#define POLLIN 0x01
#define POLLPRI 0x02
#define POLLOUT 0x04
#define POLLERR 0x08
#define POLLHUP 0x10
#define POLLNVAL 0x20
#define POLLRDNORM 0x40
#define POLLRDBAND 0x80
#define POLLWRNORM 0x100
#define POLLWRBAND 0x200
#define POLLRDHUP 0x2000
int poll(struct pollfd *, nfds_t, int);
int ppoll(struct pollfd *, nfds_t, const struct timespec *, const sigset_t *);
#endif
