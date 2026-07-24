#ifndef WBOX_WIN32_H_
#define WBOX_WIN32_H_
// internal interface of the wbox win32 port layer
#include <stdint.h>
#include <sys/types.h>

struct pollfd;

int WboxMemInit(void);
uintptr_t WboxMemLimit(void);
int WboxMemVabits(void);
// multi-window support for snapshot fork (w32mem.c)
void *WboxMemCurrentWindow(void);
uintptr_t WboxMemWindowBase(void);
void WboxMemSetWindow(void *);
int WboxMemForkWindow(void);
void WboxMemReleaseWindow(void);
uintptr_t WboxMemHandleBase(void *);
uintptr_t WboxMemHandleLimit(void *);
int WboxMemSnapshotWindow(void *srcwin, void **dstout);
void WboxMemWipeWindow(void);
int WboxMemRecommitIfOurs(void *);
// blink core hook (memorymalloc.c): drop recycled host pages in [lo,hi)
void WboxPurgeHostPagesInRange(uintptr_t lo, uintptr_t hi);

void WboxSigInit(void);

// virtual pid table for vfork-style fork children (w32proc.c)
struct W32Child;
struct W32Child *W32ChildAlloc(void);
void W32ChildAbandon(struct W32Child *);
void W32ChildPublish(struct W32Child *, void *thread_handle);
int W32ChildVpid(struct W32Child *);
int W32ChildExited(struct W32Child *);
void W32ChildSignalExec(struct W32Child *);
void W32ChildSignalExit(struct W32Child *, int status);
void W32VforkWaitParent(struct W32Child *);

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
