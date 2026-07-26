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
// destroy an arbitrary window handle (fork failure paths; the normal exit
// path uses WboxMemReleaseWindow which also switches TLS back)
void WboxMemDestroyWindow(void *win);
int WboxMemRecommitIfOurs(void *);
// MAP_SHARED|MAP_ANONYMOUS tracking across snapshot fork (w32mem.c)
void WboxShsegRegister(uintptr_t host_addr, size_t len);
void WboxShsegSyncToParent(void);
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
void W32ChildSignalKilled(struct W32Child *, int sig);
// live Machine registry: weak parent/child pointers in the vpid table are
// re-validated under the table lock before dereference (SIGCHLD UAF fix)
void W32MachineTrack(void *);
void W32MachineUntrack(void *);
int W32MachineLiveLocked(const void *);
void W32ChildReparent(void *old_machine, void *new_machine);
void W32VforkWaitParent(struct W32Child *);
// SIGCHLD delivery: the child record remembers the parent's Machine (as an
// opaque pointer) so the exit path can enqueue SIGCHLD into the parent's
// guest signal set; W32AnyChildExited lets the win32 sigsuspend polyfill
// wake even when no handler catches the signal.
void W32ChildSetParent(struct W32Child *, void *parent_machine);
void *W32ChildParent(struct W32Child *);
int W32AnyChildExited(void);
// cross-pid kill support (SysKill): find a live child's Machine by vpid
// (NULL if no such child), and force-kill a child whose host thread is
// stuck in a blocking wait that never polls guest signals.
void W32ChildSetMachine(struct W32Child *, void *child_machine);
void *W32ChildFindMachine(int vpid);
void *W32ChildFindMachineHold(int vpid);  // returns with table lock held
// C3/H1 (security-audit): raw Machine pointers in the vpid table dangle
// once the referenced Machine is freed. FreeMachine() calls
// W32ChildUnlinkMachine to drop every reference to the dying Machine;
// syscall.c brackets find+EnqueueSignal sequences with the table lock.
void W32ChildUnlinkMachine(void *machine);
void W32ChildLock(void);
void W32ChildUnlock(void);
int W32ChildTerminate(int vpid, int status);
int W32ChildHasExited(int vpid);
int W32ChildWaitExited(int vpid, int ms);
// guest death by unhandled signal inside a snapshot child: route through
// the child exit path (status 128+sig) instead of killing the host process
struct Machine;
_Noreturn void W32ChildSignalDeath(struct Machine *, int sig);

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

// ---------------------------------------------------------------- fd table
// Unified CRT-namespace fd classification (w32fd.c). The win32 port juggles
// three fd namespaces: per-System guest fds (blink/syscall.c, translated by
// W32ResolveFd there), VFS fds, and host CRT fds. This is the single
// classification entry for the CRT namespace; it subsumes the historical
// ad-hoc WboxSockIsFd / WboxEpollIsFd / IsSpecial probes that caused the
// epoll EBADF / FIONREAD / nonblock-gate bugs. Types stay opaque (HANDLE as
// void*) so win32.h needs no windows.h.
enum {
  W32FD_BAD = 0,  // not a valid CRT fd (handle lookup failed)
  W32FD_FILE,     // regular file / pipe / console: a HANDLE-backed CRT fd
  W32FD_SOCKET,   // winsock socket wrapped in a CRT fd (w32sock.c table)
  W32FD_EPOLL,    // epoll instance wrapped in a CRT fd (w32sock.c table)
  W32FD_SPECIAL,  // fake device fd sentinel 1000000..1000002 (no CRT fd)
};
struct W32FdInfo {
  int kind;
  void *handle;  // Win32 HANDLE as void*; (void*)(intptr_t)-1 == invalid
  int dev;       // sentinel number when kind == W32FD_SPECIAL
};
int W32FdClassify(int fd, struct W32FdInfo *out);

// ---------------------------------------------------------------- errno
// Centralized host-error translation tables (w32errno.c).
int W32ErrFromHost(unsigned long win32_err);  // GetLastError -> errno value
int W32ErrFromWsa(int wsa_err);               // WSAGetLastError -> errno value
int W32GaiErrFromWsa(int wsa_rc);             // ws getaddrinfo rc -> EAI_*

// ---------------------------------------------------------------- wait
// Shared wait primitive (w32fd.c): readiness for an array of CRT-namespace
// fds with a millisecond timeout (-1 = infinite). poll()/ppoll() and
// w32sock.c's epoll_wait are thin wrappers over this single entry.
int W32WaitFds(struct pollfd *pfds, unsigned long n, int timeout_ms);

// w32fd.c (F3): 64-bit-offset positioned IO. The VFS chain truncates to
// the 32-bit CRT off_t, so syscall.c routes large pread/pwrite offsets
// (>2GiB) through these instead of VfsPreadv/VfsPwritev.
struct iovec;
ssize_t W32Preadv64(int fd, const struct iovec *iov, int n, int64_t off);
ssize_t W32Pwritev64(int fd, const struct iovec *iov, int n, int64_t off);
// F3: true 64-bit file size (struct stat.st_size is 32-bit on win32)
int W32FileSize64(int fd, int64_t *out);

#endif
