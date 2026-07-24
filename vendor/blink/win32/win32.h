#ifndef WBOX_WIN32_H_
#define WBOX_WIN32_H_
// internal interface of the wbox win32 port layer
#include <stdint.h>
#include <sys/types.h>

struct pollfd;

int WboxMemInit(void);
uintptr_t WboxMemLimit(void);
int WboxMemVabits(void);

void WboxSigInit(void);

// w32sock.c (feat/net): hooks used by w32fd.c. Types kept opaque here so
// win32.h stays includable without compat socket/poll headers.
int WboxSockIsFd(int fd);
ssize_t WboxSockRead(int fd, void *buf, size_t n);
ssize_t WboxSockWrite(int fd, const void *buf, size_t n);
int WboxSockClose(int fd);                 // 1 = handled (fd was a socket)
int WboxSockFcntl(int fd, int cmd, long arg);  // -2 = not a socket
int WboxSockFstatMode(int fd, unsigned *mode); // 1 = socket, *mode set
int WboxSockFionread(int fd, int *out);        // -2 = not a socket
int WboxSockPoll(struct pollfd *pfds, unsigned long n, int timeout);
int WboxEpollIsFd(int fd);
int WboxEpollClose(int fd);                // 1 = handled (fd was epoll)

#endif
