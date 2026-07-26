// wbox win32 feat/net: sockets (Winsock2), epoll, termios console layer.
//
// Replaces the following w32stubs.c L1 stubs with strong symbols (the lead
// can delete these from w32stubs.c at merge time; until then build-mingw.sh
// compiles w32stubs.c with -D renames so the dead stubs link as wbox_stub_*):
//   sockets: socket socketpair bind connect listen accept accept4 shutdown
//            send recv sendto recvfrom sendmsg recvmsg getsockopt setsockopt
//            getsockname getpeername sockatmark inet_pton inet_ntop
//   epoll:   epoll_create epoll_create1 epoll_ctl epoll_wait epoll_pwait
//   termios: tcgetattr tcsetattr
//
// Design notes:
// - Winsock entry points are resolved lazily from ws2_32 via GetProcAddress
//   so this TU never includes winsock2.h (its sockaddr/addrinfo/etc. collide
//   with our compat ABI headers, which mirror the *Linux guest* ABI).
// - A SOCKET is wrapped in a CRT fd via _open_osfhandle so blink's
//   guest-fd==host-fd model keeps working (blink passes host fds straight
//   back to us). A side table maps CRT fd -> SOCKET + flags; w32fd.c hooks
//   read/write/close/fcntl/ioctl/fstat/poll through WboxSock*() below.
// - AF/AF_INET6: the compat ABI keeps Linux values (AF_INET6=10); Winsock
//   uses 23, so family numbers are translated both ways.
// - epoll_wait is level-triggered only (EPOLLET accepted but treated as LT,
//   recorded as a known limitation) and shares w32fd.c's poll() wait
//   primitive so pipes/files/consoles and sockets compose.
#include <errno.h>
#include <fcntl.h>
#include <arpa/inet.h>
#include <netdb.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <termios.h>
#include <unistd.h>
#include <netinet/in.h>
#include <netinet/tcp.h>

#include <windows.h>

#include "win32.h"

#ifndef DISABLE_VFS
#include "blink/hostfs.h"
#include "blink/vfs.h"
#endif

intptr_t _get_osfhandle(int);
int _open_osfhandle(intptr_t, int);
int _close(int);

#ifndef ENOTSOCK
#define ENOTSOCK 88
#endif
#ifndef EPROTONOSUPPORT
#define EPROTONOSUPPORT 93
#endif

// ---------------------------------------------------------------- winsock ABI
// Minimal self-contained Winsock2 declarations (no winsock2.h).

typedef uintptr_t WSOCK;
#define WSOCK_INVALID (~(WSOCK)0)

struct wsockaddr {
  uint16_t sa_family;
  char sa_data[14];
};

// storage-sized buffer for copying guest sockaddrs (v6 is 28 bytes)
struct wsockaddr_storage {
  uint16_t ss_family;
  char ss_pad[126];
};

struct wsa_pollfd {
  WSOCK fd;
  int16_t events;
  int16_t revents;
};

// Winsock poll event bits (differ from the compat/Linux poll.h values).
#define WSPOLLRDNORM 0x0100
#define WSPOLLRDBAND 0x0200
#define WSPOLLIN (WSPOLLRDNORM | WSPOLLRDBAND)
#define WSPOLLPRI 0x0400
#define WSPOLLWRNORM 0x0010
#define WSPOLLOUT WSPOLLWRNORM
#define WSPOLLWRBAND 0x0020
#define WSPOLLERR 0x0001
#define WSPOLLHUP 0x0002
#define WSPOLLNVAL 0x0004

#define WS_AF_INET 2
#define WS_AF_INET6 23
#define WS_SOL_SOCKET 0xffff
#define WS_SO_DEBUG 0x0001
#define WS_SO_ACCEPTCONN 0x0002
#define WS_SO_REUSEADDR 0x0004
#define WS_SO_KEEPALIVE 0x0008
#define WS_SO_DONTROUTE 0x0010
#define WS_SO_BROADCAST 0x0020
#define WS_SO_LINGER 0x0080
#define WS_SO_OOBINLINE 0x0100
#define WS_SO_SNDBUF 0x1001
#define WS_SO_RCVBUF 0x1002
#define WS_SO_SNDLOWAT 0x1003
#define WS_SO_RCVLOWAT 0x1004
#define WS_SO_SNDTIMEO 0x1005
#define WS_SO_RCVTIMEO 0x1006
#define WS_SO_ERROR 0x1007
#define WS_SO_TYPE 0x1008
#define WS_FIONBIO ((int)0x8004667e)
#define WS_FIONREAD 0x4004667f
#define WS_IP_TTL 4
#define WS_IPV6_V6ONLY 27

struct ws_addrinfo {
  int ai_flags, ai_family, ai_socktype, ai_protocol;
  size_t ai_addrlen;
  char *ai_canonname;
  struct wsockaddr *ai_addr;
  struct ws_addrinfo *ai_next;
};

struct ws_hostent {
  char *h_name;
  char **h_aliases;
  int16_t h_addrtype;
  int16_t h_length;
  char **h_addr_list;
};

typedef int(__stdcall *P_WSAStartup)(uint16_t, void *);
typedef int(__stdcall *P_WSAGetLastError)(void);
typedef void(__stdcall *P_WSASetLastError)(int);
typedef WSOCK(__stdcall *P_WSASocketW)(int, int, int, void *, unsigned,
                                       unsigned long);
typedef int(__stdcall *P_closesocket)(WSOCK);
typedef int(__stdcall *P_bind)(WSOCK, const struct wsockaddr *, int);
typedef int(__stdcall *P_connect)(WSOCK, const struct wsockaddr *, int);
typedef int(__stdcall *P_listen)(WSOCK, int);
typedef WSOCK(__stdcall *P_accept)(WSOCK, struct wsockaddr *, int *);
typedef int(__stdcall *P_send)(WSOCK, const char *, int, int);
typedef int(__stdcall *P_recv)(WSOCK, char *, int, int);
typedef int(__stdcall *P_sendto)(WSOCK, const char *, int, int,
                                 const struct wsockaddr *, int);
typedef int(__stdcall *P_recvfrom)(WSOCK, char *, int, int,
                                   struct wsockaddr *, int *);
typedef int(__stdcall *P_shutdown)(WSOCK, int);
typedef int(__stdcall *P_getsockname)(WSOCK, struct wsockaddr *, int *);
typedef int(__stdcall *P_getpeername)(WSOCK, struct wsockaddr *, int *);
typedef int(__stdcall *P_setsockopt)(WSOCK, int, int, const char *, int);
typedef int(__stdcall *P_getsockopt)(WSOCK, int, int, char *, int *);
typedef int(__stdcall *P_ioctlsocket)(WSOCK, long, unsigned long *);
typedef int(__stdcall *P_WSAPoll)(struct wsa_pollfd *, unsigned long, int);
typedef int(__stdcall *P_getaddrinfo)(const char *, const char *,
                                      const struct ws_addrinfo *,
                                      struct ws_addrinfo **);
typedef void(__stdcall *P_freeaddrinfo)(struct ws_addrinfo *);
typedef int(__stdcall *P_getnameinfo)(const struct wsockaddr *, socklen_t,
                                      char *, uint32_t, char *, uint32_t, int);
typedef int(__stdcall *P_InetPtonA)(int, const char *, void *);
typedef const char *(__stdcall *P_InetNtopA)(int, const void *, char *,
                                             size_t);
typedef struct ws_hostent *(__stdcall *P_gethostbyname)(const char *);

static struct {
  P_WSAStartup WSAStartup;
  P_WSAGetLastError WSAGetLastError;
  P_WSASetLastError WSASetLastError;
  P_WSASocketW WSASocketW;
  P_closesocket closesocket;
  P_bind bind;
  P_connect connect;
  P_listen listen;
  P_accept accept;
  P_send send;
  P_recv recv;
  P_sendto sendto;
  P_recvfrom recvfrom;
  P_shutdown shutdown;
  P_getsockname getsockname;
  P_getpeername getpeername;
  P_setsockopt setsockopt;
  P_getsockopt getsockopt;
  P_ioctlsocket ioctlsocket;
  P_WSAPoll WSAPoll;
  P_getaddrinfo getaddrinfo;
  P_freeaddrinfo freeaddrinfo;
  P_getnameinfo getnameinfo;
  P_InetPtonA InetPtonA;
  P_InetNtopA InetNtopA;
  P_gethostbyname gethostbyname;
} ws;

// debug logging: WBOX_DEBUG_NET=1 traces socket calls to stderr
static int NetDebug(void) {
  static int v = -1;
  if (v == -1) {
    const char *e = getenv("WBOX_DEBUG_NET");
    v = e && *e == '1';
  }
  return v;
}
#define NET_LOGF(...)                \
  do {                               \
    if (NetDebug()) {                \
      fprintf(stderr, "[net] " __VA_ARGS__); \
      fputc('\n', stderr);           \
    }                                \
  } while (0)

static int WsaInit(void) {
  static int state;  // 0=untried 1=ok -1=failed
  if (state) return state;
  HMODULE m = GetModuleHandleA("ws2_32.dll");
  if (!m) m = LoadLibraryA("ws2_32.dll");
  if (!m) {
    state = -1;
    return -1;
  }
#define WSGET(field, name)                                \
  do {                                                    \
    ws.field = (P_##field)GetProcAddress(m, name);        \
    if (!ws.field) {                                      \
      state = -1;                                         \
      return -1;                                          \
    }                                                     \
  } while (0)
  WSGET(WSAStartup, "WSAStartup");
  WSGET(WSAGetLastError, "WSAGetLastError");
  WSGET(WSASetLastError, "WSASetLastError");
  WSGET(WSASocketW, "WSASocketW");
  WSGET(closesocket, "closesocket");
  WSGET(bind, "bind");
  WSGET(connect, "connect");
  WSGET(listen, "listen");
  WSGET(accept, "accept");
  WSGET(send, "send");
  WSGET(recv, "recv");
  WSGET(sendto, "sendto");
  WSGET(recvfrom, "recvfrom");
  WSGET(shutdown, "shutdown");
  WSGET(getsockname, "getsockname");
  WSGET(getpeername, "getpeername");
  WSGET(setsockopt, "setsockopt");
  WSGET(getsockopt, "getsockopt");
  WSGET(ioctlsocket, "ioctlsocket");
  WSGET(WSAPoll, "WSAPoll");
  WSGET(getaddrinfo, "getaddrinfo");
  WSGET(freeaddrinfo, "freeaddrinfo");
  WSGET(getnameinfo, "getnameinfo");
  WSGET(gethostbyname, "gethostbyname");
#undef WSGET
  // optional (Vista+; present in wine): numeric address conversion
  ws.InetPtonA = (P_InetPtonA)GetProcAddress(m, "InetPtonA");
  ws.InetNtopA = (P_InetNtopA)GetProcAddress(m, "InetNtopA");
  char wsadata[512];  // WSADATA, oversized on purpose
  memset(wsadata, 0, sizeof(wsadata));
  if (ws.WSAStartup(0x0202, wsadata) != 0) {
    state = -1;
    return -1;
  }
  state = 1;
  return 1;
}

// ---------------------------------------------------------------- errno map
// mapping tables centralized in w32errno.c

static int WsaErrno(void) {
  return W32ErrFromWsa(ws.WSAGetLastError());
}

static int WsaRc(int rc) {
  if (rc == 0) return 0;
  errno = WsaErrno();
  return -1;
}

// ---------------------------------------------------------------- fd table

#define WBOX_MAXSOCK 256

static struct {
  int crtfd;
  WSOCK s;
  int nonblock;
} g_socks[WBOX_MAXSOCK];

static int SockSlot(int fd) {
  // note: zero-init table has s==0/crtfd==0 which must NOT match fd 0
  for (int i = 0; i < WBOX_MAXSOCK; ++i)
    if (g_socks[i].s != 0 && g_socks[i].s != WSOCK_INVALID &&
        g_socks[i].crtfd == fd)
      return i;
  return -1;
}

int WboxSockIsFd(int fd) {
  return SockSlot(fd) != -1;
}

static WSOCK SockFromFd(int fd) {
  int i = SockSlot(fd);
  return i != -1 ? g_socks[i].s : WSOCK_INVALID;
}

static int FamToWs(int af) {
  if (af == AF_INET6) return WS_AF_INET6;
  if (af == AF_INET) return WS_AF_INET;
  return af;  // AF_UNSPEC/AF_UNIX pass through
}

static int FamFromWs(int af) {
  if (af == WS_AF_INET6) return AF_INET6;
  return af;
}

// wrap a WSOCK into a CRT fd and register it; returns fd or -1 (errno set)
static int SockWrap(WSOCK s) {
  int slot = -1;
  for (int i = 0; i < WBOX_MAXSOCK; ++i)
    if (g_socks[i].s == 0 || g_socks[i].s == WSOCK_INVALID) {
      slot = i;
      break;
    }
  if (slot == -1) {
    ws.closesocket(s);
    errno = EMFILE;
    return -1;
  }
  int fd = _open_osfhandle((intptr_t)s, 0x8000 /*_O_BINARY*/ | 2 /*_O_RDWR*/);
  if (fd == -1) {
    ws.closesocket(s);
    errno = EMFILE;
    return -1;
  }
  g_socks[slot].crtfd = fd;
  g_socks[slot].s = s;
  g_socks[slot].nonblock = 0;
  return fd;
}

// copy winsock sockaddr -> compat sockaddr, translating family
static void SockAddrOut(struct sockaddr *dst,
                        const struct wsockaddr_storage *src) {
  memcpy(dst, src, sizeof(struct sockaddr_storage));
  dst->sa_family = (sa_family_t)FamFromWs(src->ss_family);
}

// ---------------------------------------------------------------- socket API

int socket(int domain, int type, int protocol) {
  if (WsaInit() < 0) {
    errno = EPROTONOSUPPORT;
    NET_LOGF("socket: WsaInit failed");
    return -1;
  }
  int wsd = FamToWs(domain);
  if (wsd != AF_UNIX && wsd != WS_AF_INET && wsd != WS_AF_INET6) {
    errno = EAFNOSUPPORT;
    return -1;
  }
  WSOCK s = ws.WSASocketW(wsd, type, protocol, NULL, 0, 0);
  if (s == WSOCK_INVALID) {
    errno = WsaErrno();
    NET_LOGF("socket(%d,%d,%d) -> -1 wsaerr=%d", domain, type, protocol, ws.WSAGetLastError());
    return -1;
  }
  if (wsd == WS_AF_INET6) {
    // wine creates v6 sockets v6-only; Linux guests expect dual-stack by
    // default (bindv6only=0). Force it so a ::: wildcard bind also serves
    // v4-mapped peers (e.g. `nc -l` + `nc 127.0.0.1`).
    char zero[4] = {0, 0, 0, 0};
    ws.setsockopt(s, 41 /*IPPROTO_IPV6*/, WS_IPV6_V6ONLY, zero, sizeof(zero));
  }
  int fd = SockWrap(s);
  NET_LOGF("socket(%d,%d,%d) -> fd=%d", domain, type, protocol, fd);
  return fd;
}

// Winsock has no socketpair API. Use connected loopback sockets as the
// transport; Hostfs preserves the externally visible unnamed AF_UNIX address.
static int SocketPairFail(WSOCK a, WSOCK b, WSOCK c) {
  int e = ws.WSAGetLastError();
  if (a != WSOCK_INVALID) ws.closesocket(a);
  if (b != WSOCK_INVALID) ws.closesocket(b);
  if (c != WSOCK_INVALID) ws.closesocket(c);
  ws.WSASetLastError(e);
  errno = W32ErrFromWsa(e);
  return -1;
}

static int SocketPairStream(WSOCK pair[2]) {
  WSOCK listener = WSOCK_INVALID;
  WSOCK client = WSOCK_INVALID;
  WSOCK server = WSOCK_INVALID;
  struct sockaddr_in addr, clientaddr, peeraddr;
  int len = sizeof(addr);
  memset(&addr, 0, sizeof(addr));
  addr.sin_family = WS_AF_INET;
  addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  listener = ws.WSASocketW(WS_AF_INET, SOCK_STREAM, 0, NULL, 0, 0);
  if (listener == WSOCK_INVALID ||
      ws.bind(listener, (struct wsockaddr *)&addr, sizeof(addr)) ||
      ws.getsockname(listener, (struct wsockaddr *)&addr, &len) ||
      ws.listen(listener, 1)) {
    return SocketPairFail(listener, client, server);
  }
  client = ws.WSASocketW(WS_AF_INET, SOCK_STREAM, 0, NULL, 0, 0);
  if (client == WSOCK_INVALID ||
      ws.connect(client, (struct wsockaddr *)&addr, len)) {
    return SocketPairFail(listener, client, server);
  }
  len = sizeof(clientaddr);
  if (ws.getsockname(client, (struct wsockaddr *)&clientaddr, &len)) {
    return SocketPairFail(listener, client, server);
  }
  for (;;) {
    int peerlen = sizeof(peeraddr);
    server = ws.accept(listener, (struct wsockaddr *)&peeraddr, &peerlen);
    if (server == WSOCK_INVALID) {
      return SocketPairFail(listener, client, server);
    }
    if (peeraddr.sin_family == clientaddr.sin_family &&
        peeraddr.sin_port == clientaddr.sin_port &&
        peeraddr.sin_addr.s_addr == clientaddr.sin_addr.s_addr) {
      break;
    }
    ws.closesocket(server);
    server = WSOCK_INVALID;
  }
  ws.closesocket(listener);
  pair[0] = server;
  pair[1] = client;
  return 0;
}

static int SocketPairDatagram(WSOCK pair[2]) {
  WSOCK a = WSOCK_INVALID;
  WSOCK b = WSOCK_INVALID;
  struct sockaddr_in aa, ba;
  int alen = sizeof(aa);
  int blen = sizeof(ba);
  memset(&aa, 0, sizeof(aa));
  memset(&ba, 0, sizeof(ba));
  aa.sin_family = ba.sin_family = WS_AF_INET;
  aa.sin_addr.s_addr = ba.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  a = ws.WSASocketW(WS_AF_INET, SOCK_DGRAM, 0, NULL, 0, 0);
  b = ws.WSASocketW(WS_AF_INET, SOCK_DGRAM, 0, NULL, 0, 0);
  if (a == WSOCK_INVALID || b == WSOCK_INVALID ||
      ws.bind(a, (struct wsockaddr *)&aa, sizeof(aa)) ||
      ws.bind(b, (struct wsockaddr *)&ba, sizeof(ba)) ||
      ws.getsockname(a, (struct wsockaddr *)&aa, &alen) ||
      ws.getsockname(b, (struct wsockaddr *)&ba, &blen) ||
      ws.connect(a, (struct wsockaddr *)&ba, blen) ||
      ws.connect(b, (struct wsockaddr *)&aa, alen)) {
    return SocketPairFail(a, b, WSOCK_INVALID);
  }
  pair[0] = a;
  pair[1] = b;
  return 0;
}

int socketpair(int domain, int type, int protocol, int fds[2]) {
  WSOCK pair[2];
  int rc;
  if (!fds) {
    errno = EFAULT;
    return -1;
  }
  if (domain != AF_UNIX) {
    errno = EAFNOSUPPORT;
    return -1;
  }
  if (protocol) {
    errno = EPROTONOSUPPORT;
    return -1;
  }
  if (WsaInit() < 0) {
    errno = EPROTONOSUPPORT;
    return -1;
  }
  if (type == SOCK_STREAM) {
    rc = SocketPairStream(pair);
  } else if (type == SOCK_DGRAM) {
    rc = SocketPairDatagram(pair);
  } else {
    errno = ESOCKTNOSUPPORT;
    return -1;
  }
  if (rc == -1) return -1;
  fds[0] = SockWrap(pair[0]);
  if (fds[0] == -1) {
    ws.closesocket(pair[1]);
    return -1;
  }
  fds[1] = SockWrap(pair[1]);
  if (fds[1] == -1) {
    WboxSockClose(fds[0]);
    return -1;
  }
  return 0;
}

int bind(int fd, const struct sockaddr *addr, socklen_t len) {
  WSOCK s = SockFromFd(fd);
  if (s == WSOCK_INVALID) {
    errno = ENOTSOCK;
    return -1;
  }
  struct wsockaddr_storage wa;
  if (len > (socklen_t)sizeof(wa)) len = sizeof(wa);
  memcpy(&wa, addr, len);
  wa.ss_family = (uint16_t)FamToWs(wa.ss_family);
  int rc = WsaRc(ws.bind(s, (struct wsockaddr *)&wa, len));
  NET_LOGF("bind(fd=%d fam=%d) -> %d wsaerr=%d", fd, wa.ss_family, rc, ws.WSAGetLastError());
  return rc;
}

int connect(int fd, const struct sockaddr *addr, socklen_t len) {
  WSOCK s = SockFromFd(fd);
  if (s == WSOCK_INVALID) {
    errno = ENOTSOCK;
    return -1;
  }
  struct wsockaddr_storage wa;
  if (len > (socklen_t)sizeof(wa)) len = sizeof(wa);
  memcpy(&wa, addr, len);
  wa.ss_family = (uint16_t)FamToWs(wa.ss_family);
  if (ws.connect(s, (struct wsockaddr *)&wa, len) != 0) {
    int e = WsaErrno();
    NET_LOGF("connect(fd=%d fam=%d) -> -1 wsaerr=%d", fd, wa.ss_family, ws.WSAGetLastError());
    if (e == EAGAIN) e = EINPROGRESS;  // nonblocking connect in flight
    errno = e;
    return -1;
  }
  NET_LOGF("connect(fd=%d fam=%d) -> 0", fd, wa.ss_family);
  return 0;
}

int listen(int fd, int backlog) {
  WSOCK s = SockFromFd(fd);
  if (s == WSOCK_INVALID) {
    errno = ENOTSOCK;
    return -1;
  }
  int rc = WsaRc(ws.listen(s, backlog));
  NET_LOGF("listen(fd=%d) -> %d wsaerr=%d", fd, rc, ws.WSAGetLastError());
  return rc;
}

int accept(int fd, struct sockaddr *addr, socklen_t *len) {
  return accept4(fd, addr, len, 0);
}

int accept4(int fd, struct sockaddr *addr, socklen_t *len, int flags) {
  WSOCK s = SockFromFd(fd);
  if (s == WSOCK_INVALID) {
    errno = ENOTSOCK;
    return -1;
  }
  struct wsockaddr_storage wa;
  int wlen = sizeof(wa);
  WSOCK a = ws.accept(s, addr ? (struct wsockaddr *)&wa : NULL,
                      addr ? &wlen : NULL);
  if (a == WSOCK_INVALID) {
    errno = WsaErrno();
    return -1;
  }
  if (addr && len) {
    SockAddrOut(addr, &wa);
    *len = wlen;
  }
  int nfd = SockWrap(a);
  if (nfd == -1) return -1;
  if (flags & SOCK_NONBLOCK) {
    unsigned long one = 1;
    ws.ioctlsocket(a, WS_FIONBIO, &one);
    g_socks[SockSlot(nfd)].nonblock = 1;
  }
  return nfd;
}

int shutdown(int fd, int how) {
  WSOCK s = SockFromFd(fd);
  if (s == WSOCK_INVALID) {
    errno = ENOTSOCK;
    return -1;
  }
  if (how < 0 || how > 2) {
    errno = EINVAL;
    return -1;
  }
  return WsaRc(ws.shutdown(s, how));  // SHUT_RD/WR/RDWR == SD_RECEIVE/SEND/BOTH
}

int getsockname(int fd, struct sockaddr *addr, socklen_t *len) {
  WSOCK s = SockFromFd(fd);
  if (s == WSOCK_INVALID) {
    errno = ENOTSOCK;
    return -1;
  }
  struct wsockaddr_storage wa;
  int wlen = sizeof(wa);
  if (ws.getsockname(s, (struct wsockaddr *)&wa, &wlen) != 0) {
    NET_LOGF("getsockname(fd=%d) -> -1 wsaerr=%d", fd, ws.WSAGetLastError());
    return WsaRc(-1);
  }
  SockAddrOut(addr, &wa);
  *len = wlen;
  return 0;
}

int getpeername(int fd, struct sockaddr *addr, socklen_t *len) {
  WSOCK s = SockFromFd(fd);
  if (s == WSOCK_INVALID) {
    errno = ENOTSOCK;
    return -1;
  }
  struct wsockaddr_storage wa;
  int wlen = sizeof(wa);
  if (ws.getpeername(s, (struct wsockaddr *)&wa, &wlen) != 0) return WsaRc(-1);
  SockAddrOut(addr, &wa);
  *len = wlen;
  return 0;
}

// ---------------------------------------------------------------- io on sockets

void WboxEpollRefreshFd(int fd);  // defined in the epoll section below

// winsock send/recv flags: MSG_OOB=1 MSG_PEEK=2 MSG_DONTROUTE=4 match the
// compat low bits; MSG_NOSIGNAL/MSG_DONTWAIT/etc must be stripped.
static int WsMsgFlags(int flags) {
  return flags & (MSG_OOB | MSG_PEEK | MSG_DONTROUTE);
}

// returns 1 when a MSG_DONTWAIT-style op should fail with EAGAIN right now
static int SockWouldBlock(WSOCK s, int16_t events) {
  struct wsa_pollfd p;
  p.fd = s;
  p.events = events;
  p.revents = 0;
  if (ws.WSAPoll(&p, 1, 0) <= 0) return 1;
  return !(p.revents & (events | WSPOLLERR | WSPOLLHUP));
}

ssize_t send(int fd, const void *buf, size_t n, int flags) {
  return sendto(fd, buf, n, flags, NULL, 0);
}

ssize_t sendto(int fd, const void *buf, size_t n, int flags,
               const struct sockaddr *addr, socklen_t len) {
  WSOCK s = SockFromFd(fd);
  if (s == WSOCK_INVALID) {
    errno = ENOTSOCK;
    return -1;
  }
  if ((flags & MSG_DONTWAIT) && SockWouldBlock(s, WSPOLLOUT)) {
    errno = EAGAIN;
    return -1;
  }
  struct wsockaddr_storage wa;
  if (addr) {
    if (len > (socklen_t)sizeof(wa)) len = sizeof(wa);
    memcpy(&wa, addr, len);
    wa.ss_family = (uint16_t)FamToWs(wa.ss_family);
  }
  if (n > 0x7fffffff) n = 0x7fffffff;
  int rc = ws.sendto(s, buf, (int)n, WsMsgFlags(flags),
                     addr ? (struct wsockaddr *)&wa : NULL, addr ? (int)len : 0);
  if (rc < 0) {
    errno = WsaErrno();
    NET_LOGF("sendto(fd=%d n=%d fl=%x) -> -1 wsaerr=%d", fd, (int)n, flags, ws.WSAGetLastError());
    WboxEpollRefreshFd(fd);
    return -1;
  }
  NET_LOGF("sendto(fd=%d n=%d fl=%x) -> %d", fd, (int)n, flags, rc);
  WboxEpollRefreshFd(fd);
  return rc;
}

ssize_t recv(int fd, void *buf, size_t n, int flags) {
  return recvfrom(fd, buf, n, flags, NULL, NULL);
}

ssize_t recvfrom(int fd, void *buf, size_t n, int flags, struct sockaddr *addr,
                 socklen_t *len) {
  WSOCK s = SockFromFd(fd);
  if (s == WSOCK_INVALID) {
    errno = ENOTSOCK;
    return -1;
  }
  if ((flags & MSG_DONTWAIT) && SockWouldBlock(s, WSPOLLIN)) {
    errno = EAGAIN;
    return -1;
  }
  struct wsockaddr_storage wa;
  int wlen = sizeof(wa);
  if (n > 0x7fffffff) n = 0x7fffffff;
  // Linux: a zero-length receive returns 0 immediately and leaves any
  // pending datagram untouched; Windows instead fails WSAEMSGSIZE and
  // discards it (glibc's resolver issues such recvfrom(fd, _, 0, 0) as a
  // size probe for the second parallel A/AAAA answer — see WIN32-PORT.md
  // known-issues: answering with the FIONREAD pending size here exposed a
  // separate child-window commit bug, so keep the safe Linux behavior).
  if (!n) return 0;
  int rc = ws.recvfrom(s, buf, (int)n, WsMsgFlags(flags),
                       addr ? (struct wsockaddr *)&wa : NULL,
                       addr ? &wlen : NULL);
  if (rc < 0) {
    int werr = ws.WSAGetLastError();
    if (getenv("WBOX_DEBUG_NET"))
      fprintf(stderr, "wbox recvfrom ERR(fd=%d n=%d fl=%#x) werr=%d\n", fd,
              (int)n, flags, werr);
    // Linux UDP semantics: an oversized datagram is TRUNCATED to the
    // buffer and its length returned; Windows instead fails with
    // WSAEMSGSIZE and drops it. glibc's resolver depends on the Linux
    // behavior (it polls then recvfrom()s into a fixed 512B/64KB buffer
    // and treats EMSGSIZE errors as fatal), so translate: report a full
    // buffer. With MSG_TRUNC the caller asked for the untruncated size;
    // it is unrecoverable here, so a full buffer is the best estimate.
    if (werr == 10040 /*WSAEMSGSIZE*/) {
      // glibc probes the true datagram size with a zero-length
      // MSG_PEEK|MSG_TRUNC recvfrom; FIONREAD reports the pending
      // datagram size. Otherwise report a full (truncated) buffer.
      unsigned long pending = 0;
      if (!ws.ioctlsocket(s, WS_FIONREAD, &pending) && pending) {
        NET_LOGF("recvfrom(fd=%d) WSAEMSGSIZE -> pending %lu", fd, pending);
        if (addr && len) {
          SockAddrOut(addr, &wa);
          *len = wlen;
        }
        WboxEpollRefreshFd(fd);
        return (ssize_t)pending;
      }
      NET_LOGF("recvfrom(fd=%d) WSAEMSGSIZE -> truncate to %d", fd, (int)n);
      if (addr && len) {
        SockAddrOut(addr, &wa);
        *len = wlen;
      }
      WboxEpollRefreshFd(fd);
      return (ssize_t)n;
    }
    errno = WsaErrno();
    NET_LOGF("recvfrom(fd=%d fl=%x) -> -1 wsaerr=%d", fd, flags, werr);
    WboxEpollRefreshFd(fd);
    return -1;
  }
  NET_LOGF("recvfrom(fd=%d fl=%x) -> %d", fd, flags, rc);
  if (getenv("WBOX_DEBUG_NET"))
    fprintf(stderr, "wbox recvfrom(fd=%d n=%d fl=%#x) rc=%d wlen=%d\n", fd,
            (int)n, flags, rc, wlen);
  if (addr && len) {
    SockAddrOut(addr, &wa);
    *len = wlen;
  }
  WboxEpollRefreshFd(fd);
  return rc;
}

// scatter/gather: flatten (send) / linearize+scatter (recv); ancillary data
// is dropped (DISABLE_ANCILLARY means blink never passes fds over sockets).
ssize_t sendmsg(int fd, const struct msghdr *msg, int flags) {
  size_t total = 0;
  for (size_t i = 0; i < msg->msg_iovlen; ++i) total += msg->msg_iov[i].iov_len;
  char stackbuf[8192];
  char *buf = stackbuf;
  if (total > sizeof(stackbuf)) {
    buf = malloc(total ? total : 1);
    if (!buf) {
      errno = ENOMEM;
      return -1;
    }
  }
  size_t off = 0;
  for (size_t i = 0; i < msg->msg_iovlen; ++i) {
    memcpy(buf + off, msg->msg_iov[i].iov_base, msg->msg_iov[i].iov_len);
    off += msg->msg_iov[i].iov_len;
  }
  ssize_t rc = sendto(fd, buf, total, flags,
                      (const struct sockaddr *)msg->msg_name,
                      msg->msg_namelen);
  if (buf != stackbuf) free(buf);
  return rc;
}

ssize_t recvmsg(int fd, struct msghdr *msg, int flags) {
  size_t total = 0;
  for (size_t i = 0; i < msg->msg_iovlen; ++i) total += msg->msg_iov[i].iov_len;
  // fast path: single iov
  if (msg->msg_iovlen == 1) {
    ssize_t rc = recvfrom(fd, msg->msg_iov[0].iov_base, total, flags,
                          (struct sockaddr *)msg->msg_name,
                          msg->msg_name ? &msg->msg_namelen : NULL);
    if (rc >= 0) msg->msg_flags = 0;
    return rc;
  }
  char stackbuf[8192];
  char *buf = stackbuf;
  if (total > sizeof(stackbuf)) {
    buf = malloc(total ? total : 1);
    if (!buf) {
      errno = ENOMEM;
      return -1;
    }
  }
  socklen_t namelen = msg->msg_namelen;
  ssize_t rc = recvfrom(fd, buf, total, flags,
                        (struct sockaddr *)msg->msg_name,
                        msg->msg_name ? &namelen : NULL);
  if (rc >= 0) {
    size_t off = 0, left = (size_t)rc;
    for (size_t i = 0; i < msg->msg_iovlen && left; ++i) {
      size_t c = msg->msg_iov[i].iov_len < left ? msg->msg_iov[i].iov_len : left;
      memcpy(msg->msg_iov[i].iov_base, buf + off, c);
      off += c;
      left -= c;
    }
    msg->msg_namelen = namelen;
    msg->msg_flags = 0;
  }
  if (buf != stackbuf) free(buf);
  return rc;
}

int sockatmark(int fd) {
  WSOCK s = SockFromFd(fd);
  if (s == WSOCK_INVALID) {
    errno = ENOTSOCK;
    return -1;
  }
  unsigned long atmark = 0;
  if (ws.ioctlsocket(s, 0x40047307 /*SIOCATMARK*/, &atmark) != 0)
    return WsaRc(-1);
  return atmark ? 1 : 0;
}

// ---------------------------------------------------------------- sockopts

struct ws_sockaddr_in6_dummy;  // unused

static int SockLevelToWs(int level) {
  if (level == SOL_SOCKET) return WS_SOL_SOCKET;
  return level;  // IPPROTO_IP/TCP/UDP/IPV6 numeric values match
}

// compat (Linux) SOL_SOCKET optname -> winsock; -1 = unsupported
static int SockOptToWs(int level, int opt) {
  if (level == SOL_SOCKET) {
    switch (opt) {
      case SO_DEBUG: return WS_SO_DEBUG;
      case SO_REUSEADDR: return WS_SO_REUSEADDR;
      case SO_TYPE: return WS_SO_TYPE;
      case SO_ERROR: return WS_SO_ERROR;
      case SO_DONTROUTE: return WS_SO_DONTROUTE;
      case SO_BROADCAST: return WS_SO_BROADCAST;
      case SO_SNDBUF: return WS_SO_SNDBUF;
      case SO_RCVBUF: return WS_SO_RCVBUF;
      case SO_KEEPALIVE: return WS_SO_KEEPALIVE;
      case SO_OOBINLINE: return WS_SO_OOBINLINE;
      case SO_LINGER: return WS_SO_LINGER;
      case SO_RCVLOWAT: return WS_SO_RCVLOWAT;
      case SO_SNDLOWAT: return WS_SO_SNDLOWAT;
      case SO_RCVTIMEO: return WS_SO_RCVTIMEO;
      case SO_SNDTIMEO: return WS_SO_SNDTIMEO;
      case SO_ACCEPTCONN: return WS_SO_ACCEPTCONN;
      default: return -1;
    }
  }
  if (level == IPPROTO_IP && opt == IP_TTL) return WS_IP_TTL;
  if (level == IPPROTO_IPV6 && opt == IPV6_V6ONLY) return WS_IPV6_V6ONLY;
  if (level == IPPROTO_TCP && opt == TCP_NODELAY) return TCP_NODELAY;
  return opt;  // numeric passthrough for the rest (best effort)
}

int setsockopt(int fd, int level, int optname, const void *optval,
               socklen_t optlen) {
  WSOCK s = SockFromFd(fd);
  if (s == WSOCK_INVALID) {
    errno = ENOTSOCK;
    return -1;
  }
  // SO_REUSEPORT: no winsock equivalent; treat as success (reuse semantics
  // are close enough to SO_REUSEADDR which guests usually set anyway).
  if (level == SOL_SOCKET && optname == SO_REUSEPORT) return 0;
  // SO_TIMESTAMP*: no winsock equivalent; accept-and-ignore so curl/wget
  // don't fatal on it.
  if (level == SOL_SOCKET && (optname == SO_TIMESTAMP || optname == SO_TIMESTAMPNS))
    return 0;
  // IP_RECVERR/IPV6_RECVERR (glibc's DNS resolver sets these on its UDP
  // sockets and treats failure as fatal): no winsock equivalent; accept as
  // no-op so resolution proceeds (ICMP errors just won't be reported).
  if ((level == IPPROTO_IP && optname == IP_RECVERR) ||
      (level == IPPROTO_IPV6 && optname == IPV6_RECVERR))
    return 0;
  // IPV6_V6ONLY: accept-and-ignore. Winsock dual-mode sockets already accept
  // v4-mapped connections by default; forwarding this option (in particular
  // setting it to 1, or wine failing the underlying call) breaks dual-stack
  // listeners like `nc -l` bound to ::: an IPv4 peer then gets refused.
  if (level == IPPROTO_IPV6 && optname == IPV6_V6ONLY) return 0;
  int wsopt = SockOptToWs(level, optname);
  if (wsopt == -1) {
    errno = ENOPROTOOPT;
    return -1;
  }
  // timeouts: guest passes struct timeval, winsock wants DWORD milliseconds
  DWORD msbuf;
  const void *vp = optval;
  int vlen = optlen;
  if (level == SOL_SOCKET &&
      (optname == SO_RCVTIMEO || optname == SO_SNDTIMEO)) {
    if (optlen >= (socklen_t)(2 * sizeof(long))) {
      const long *tv = optval;  // struct timeval {long tv_sec; long tv_usec;}
      int64_t ms = (int64_t)tv[0] * 1000 + tv[1] / 1000;
      msbuf = (DWORD)(ms > 0x7fffffff ? 0x7fffffff : ms);
    } else {
      msbuf = 0;
    }
    vp = &msbuf;
    vlen = sizeof(msbuf);
  }
  int rc = WsaRc(ws.setsockopt(s, SockLevelToWs(level), wsopt, vp, vlen));
  NET_LOGF("setsockopt(fd=%d lvl=%d opt=%d) -> %d wsaerr=%d", fd, level, optname, rc, ws.WSAGetLastError());
  return rc;
}

int getsockopt(int fd, int level, int optname, void *optval,
               socklen_t *optlen) {
  WSOCK s = SockFromFd(fd);
  if (s == WSOCK_INVALID) {
    errno = ENOTSOCK;
    return -1;
  }
  // IPV6_V6ONLY: no-op dual-stack socket always reports 0 (v4-mapped ok)
  if (level == IPPROTO_IPV6 && optname == IPV6_V6ONLY) {
    if (*optlen >= (socklen_t)sizeof(int)) {
      memset(optval, 0, sizeof(int));
      *optlen = sizeof(int);
    }
    return 0;
  }
  int wsopt = SockOptToWs(level, optname);
  if (wsopt == -1) {
    errno = ENOPROTOOPT;
    return -1;
  }
  char vbuf[64];
  int vlen = sizeof(vbuf);
  if (ws.getsockopt(s, SockLevelToWs(level), wsopt, vbuf, &vlen) != 0)
    return WsaRc(-1);
  if (level == SOL_SOCKET &&
      (optname == SO_RCVTIMEO || optname == SO_SNDTIMEO)) {
    // DWORD ms -> struct timeval
    DWORD ms;
    memcpy(&ms, vbuf, sizeof(ms));
    long *tv = optval;
    tv[0] = ms / 1000;
    tv[1] = (ms % 1000) * 1000;
    *optlen = 2 * sizeof(long);
    return 0;
  }
  socklen_t want = *optlen;
  socklen_t c = (socklen_t)vlen < want ? (socklen_t)vlen : want;
  memcpy(optval, vbuf, c);
  *optlen = vlen;
  return 0;
}

// ---------------------------------------------------------------- w32fd hooks

// called from w32fd.c read(); socket fds only
ssize_t WboxSockRead(int fd, void *buf, size_t n) {
  return recv(fd, buf, n, 0);
}

ssize_t WboxSockWrite(int fd, const void *buf, size_t n) {
  return send(fd, buf, n, 0);
}

// called from w32fd.c close(); returns 1 when fd was a socket (handled)
void WboxEpollPurgeFd(int fd);  // defined in the epoll section below

int WboxSockClose(int fd) {
  int i = SockSlot(fd);
  if (i == -1) return 0;
  NET_LOGF("sock close(fd=%d)", fd);
  WboxEpollPurgeFd(fd);  // Linux drops closed fds from all epoll sets
  WSOCK s = g_socks[i].s;
  g_socks[i].s = WSOCK_INVALID;
  g_socks[i].crtfd = -1;
  ws.closesocket(s);
  _close(fd);  // release CRT slot; CloseHandle on the dead handle may fail
  return 1;
}

// called from w32fd.c fcntl(); returns -2 when fd is not a socket
int WboxSockFcntl(int fd, int cmd, long arg) {
  int i = SockSlot(fd);
  if (i == -1) return -2;
  NET_LOGF("sock fcntl(fd=%d cmd=%d arg=%ld)", fd, cmd, arg);
  switch (cmd) {
    case F_SETFL: {
      unsigned long nb = (arg & O_NDELAY) ? 1 : 0;
      if (ws.ioctlsocket(g_socks[i].s, WS_FIONBIO, &nb) != 0)
        return WsaRc(-1);
      g_socks[i].nonblock = (int)nb;
      return 0;
    }
    case F_GETFL:
      return O_RDWR | (g_socks[i].nonblock ? O_NONBLOCK : 0);
    case F_GETFD:
    case F_SETFD:
      return 0;
    default:
      errno = EINVAL;
      return -1;
  }
}

// called from w32fd.c fstat(); returns 1 and sets *mode when socket
int WboxSockFstatMode(int fd, unsigned *mode) {
  if (SockSlot(fd) == -1) return 0;
  *mode = S_IFSOCK | 0666;
  return 1;
}

// called from w32fd.c ioctl(FIONREAD); returns -2 when fd is not a socket
int WboxSockFionread(int fd, int *out) {
  int i = SockSlot(fd);
  if (i == -1) return -2;
  unsigned long avail = 0;
  if (ws.ioctlsocket(g_socks[i].s, WS_FIONREAD, &avail) != 0)
    return WsaRc(-1);
  *out = (int)avail;
  return 0;
}

// ---------------------------------------------------------------- poll bridge
// Used by w32fd.c poll(): waits on the SOCKET-backed entries of a pollfd
// array via WSAPoll. revents of non-socket entries are left untouched.

static int16_t PollEventsToWs(int16_t ev) {
  int16_t r = 0;
  if (ev & POLLIN) r |= WSPOLLIN;
  if (ev & POLLPRI) r |= WSPOLLPRI;
  if (ev & POLLOUT) r |= WSPOLLOUT;
  return r;
}

static int16_t PollEventsFromWs(int16_t ev) {
  int16_t r = 0;
  if (ev & WSPOLLIN) r |= POLLIN;
  if (ev & WSPOLLPRI) r |= POLLPRI;
  if (ev & WSPOLLOUT) r |= POLLOUT;
  if (ev & WSPOLLERR) r |= POLLERR;
  if (ev & WSPOLLHUP) r |= POLLHUP;
  if (ev & WSPOLLNVAL) r |= POLLNVAL;
  return r;
}

// returns number of socket fds with events; -1 on hard error
int WboxSockPoll(struct pollfd *pfds, nfds_t n, int timeout) {
  if (WsaInit() < 0) return 0;
  NET_LOGF("WboxSockPoll(nfds=%lu timeout=%d)", (unsigned long)n, timeout);
  struct wsa_pollfd stackw[32];
  int stackidx[32];
  struct wsa_pollfd *w = stackw;
  int *idx = stackidx;
  int nsock = 0;
  for (nfds_t i = 0; i < n; ++i)
    if (pfds[i].fd >= 0 && WboxSockIsFd(pfds[i].fd)) ++nsock;
  if (!nsock) return 0;
  if (nsock > (int)(sizeof(stackw) / sizeof(stackw[0]))) {
    w = malloc(nsock * sizeof(*w) + nsock * sizeof(int));
    if (!w) {
      errno = ENOMEM;
      return -1;
    }
    idx = (int *)(w + nsock);
  }
  int k = 0;
  for (nfds_t i = 0; i < n; ++i) {
    if (pfds[i].fd >= 0 && WboxSockIsFd(pfds[i].fd)) {
      w[k].fd = SockFromFd(pfds[i].fd);
      w[k].events = PollEventsToWs(pfds[i].events);
      w[k].revents = 0;
      idx[k] = (int)i;
      ++k;
    }
  }
  /* analyzer: idx[0..k) is fully initialized here and k == nsock
     (same filter as the count pass above); the "uninitialized
     subscript" warning below is a false positive. */
  int rc = ws.WSAPoll(w, nsock, timeout);
  if (rc < 0) {
    if (w != stackw) free(w);
    errno = WsaErrno();
    return -1;
  }
  int ready = 0;
  for (int j = 0; j < nsock; ++j) {
    if (w[j].revents) {
      pfds[idx[j]].revents |= PollEventsFromWs(w[j].revents);
      if (pfds[idx[j]].revents) ++ready;
    }
  }
  if (w != stackw) free(w);
  return ready;
}

// ---------------------------------------------------------------- epoll
// epoll over w32fd.c's shared poll primitive. Level-triggered,
// edge-triggered and one-shot registrations share one interest table.

#define WBOX_MAXEPOLL 32
#define WBOX_MAXEPOLLMAP 1024

struct WboxEpollEnt {
  int fd;
  uint32_t events;
  epoll_data_t data;
  int oneshot_armed;  // EPOLLONESHOT: already delivered once
  uint32_t edge_ready;
};

struct WboxEpoll {
  int used;
  int refs;
  int n, cap;
  struct WboxEpollEnt *ents;
};

struct WboxEpollMap {
  int used;
  int fd;
  struct WboxEpoll *ep;
};

static struct WboxEpoll g_epolls[WBOX_MAXEPOLL];
static struct WboxEpollMap g_epollmaps[WBOX_MAXEPOLLMAP];

static struct WboxEpoll *EpollFromFd(int fd) {
  for (int i = 0; i < WBOX_MAXEPOLLMAP; ++i)
    if (g_epollmaps[i].used && g_epollmaps[i].fd == fd)
      return g_epollmaps[i].ep;
  return NULL;
}

static int EpollMapFd(int fd, struct WboxEpoll *ep) {
  for (int i = 0; i < WBOX_MAXEPOLLMAP; ++i) {
    if (!g_epollmaps[i].used) {
      g_epollmaps[i].used = 1;
      g_epollmaps[i].fd = fd;
      g_epollmaps[i].ep = ep;
      ++ep->refs;
      return 0;
    }
  }
  errno = EMFILE;
  return -1;
}

int WboxEpollIsFd(int fd) {
  return EpollFromFd(fd) != NULL;
}

// Linux semantics: closing an fd removes it from every epoll interest list
// (once the last fd referencing the file description is gone).
void WboxEpollPurgeFd(int fd) {
  for (int i = 0; i < WBOX_MAXEPOLL; ++i) {
    struct WboxEpoll *ep = &g_epolls[i];
    if (!ep->used) continue;
    for (int j = 0; j < ep->n;)
      if (ep->ents[j].fd == fd)
        ep->ents[j] = ep->ents[--ep->n];
      else
        ++j;
  }
}

// called from w32fd.c close(); returns 1 when fd was an epoll fd
int WboxEpollClose(int fd) {
  for (int i = 0; i < WBOX_MAXEPOLLMAP; ++i) {
    if (g_epollmaps[i].used && g_epollmaps[i].fd == fd) {
      struct WboxEpoll *ep = g_epollmaps[i].ep;
      memset(&g_epollmaps[i], 0, sizeof(g_epollmaps[i]));
      _close(fd);  // releases CRT slot and the dummy NUL handle
      if (!--ep->refs) {
        free(ep->ents);
        memset(ep, 0, sizeof(*ep));
      }
      return 1;
    }
  }
  return 0;
}

int WboxEpollDup(int fd) {
  struct WboxEpoll *ep = EpollFromFd(fd);
  HANDLE oldh, newh;
  int newfd;
  if (!ep) return -2;
  oldh = (HANDLE)_get_osfhandle(fd);
  if (oldh == INVALID_HANDLE_VALUE) {
    errno = EBADF;
    return -1;
  }
  if (!DuplicateHandle(GetCurrentProcess(), oldh, GetCurrentProcess(), &newh,
                       0, FALSE, DUPLICATE_SAME_ACCESS)) {
    errno = W32ErrFromHost(GetLastError());
    return -1;
  }
  newfd = _open_osfhandle((intptr_t)newh, 0x8000 | 2);
  if (newfd == -1) {
    CloseHandle(newh);
    errno = EMFILE;
    return -1;
  }
  if (EpollMapFd(newfd, ep) == -1) {
    _close(newfd);
    return -1;
  }
  return newfd;
}

int epoll_create1(int flags) {
  (void)flags;  // EPOLL_CLOEXEC is a no-op without exec
  for (int i = 0; i < WBOX_MAXEPOLL; ++i) {
    if (!g_epolls[i].used) {
      // dummy backing handle: NUL device (event handles are rejected by
      // wine's _open_osfhandle); the handle is never used for io.
      HANDLE ev = CreateFileW(L"NUL", GENERIC_READ | GENERIC_WRITE,
                              FILE_SHARE_READ | FILE_SHARE_WRITE, NULL,
                              OPEN_EXISTING, 0, NULL);
      if (ev == INVALID_HANDLE_VALUE) {
        errno = EMFILE;
        return -1;
      }
      int fd = _open_osfhandle((intptr_t)ev, 0x8000 | 2);
      if (fd == -1) {
        CloseHandle(ev);
        errno = EMFILE;
        return -1;
      }
      memset(&g_epolls[i], 0, sizeof(g_epolls[i]));
      g_epolls[i].used = 1;
      if (EpollMapFd(fd, &g_epolls[i]) == -1) {
        memset(&g_epolls[i], 0, sizeof(g_epolls[i]));
        _close(fd);
        return -1;
      }
#ifndef DISABLE_VFS
      // win32 VFS 模式下存在两个宿主侧 fd 命名空间：VFS 号与裸 CRT fd。
      // syscall.c 的 W32EpollHostFd 把 guest fd 先翻成 Fd 表 hostfd，再按
      // VFS 号解析 HostfsInfo.filefd；裸 CRT fd 与 VFS 号都从 0 递增、
      // 必然撞车，因此 epoll fd 也注册进 VFS 表，与文件/管道/socket 走同
      // 一条翻译路径。返回值变为 VFS 号；CRT fd 留在 HostfsInfo.filefd，
      // 关闭路径 VfsClose -> HostfsClose -> close(crtfd) -> WboxEpollClose
      // 仍照常回收本表项。
      {
        struct VfsInfo *info = NULL;
        int vfd;
        if (HostfsWrapFd(fd, false, &info) == -1) {
          WboxEpollClose(fd);
          errno = EMFILE;
          return -1;
        }
        if ((vfd = VfsAddFd(info)) == -1) {
          VfsFreeInfo(info);
          errno = EMFILE;
          return -1;
        }
        return vfd;
      }
#else
      return fd;
#endif
    }
  }
  errno = EMFILE;
  return -1;
}

int epoll_create(int size) {
  if (size <= 0) {
    errno = EINVAL;
    return -1;
  }
  return epoll_create1(0);
}

int epoll_ctl(int epfd, int op, int fd, struct epoll_event *ev) {
  if (!WboxEpollIsFd(epfd)) {
    errno = EBADF;
    return -1;
  }
  struct WboxEpoll *ep = EpollFromFd(epfd);
  int idx = -1;
  for (int i = 0; i < ep->n; ++i)
    if (ep->ents[i].fd == fd) {
      idx = i;
      break;
    }
  switch (op) {
    case EPOLL_CTL_ADD:
      if (idx != -1) {
        errno = EEXIST;
        return -1;
      }
      if (!ev) {
        errno = EINVAL;
        return -1;
      }
      if (ep->n == ep->cap) {
        int ncap = ep->cap ? ep->cap * 2 : 16;
        struct WboxEpollEnt *ne = realloc(ep->ents, ncap * sizeof(*ne));
        if (!ne) {
          errno = ENOMEM;
          return -1;
        }
        ep->ents = ne;
        ep->cap = ncap;
      }
      ep->ents[ep->n].fd = fd;
      ep->ents[ep->n].events = ev->events | EPOLLERR | EPOLLHUP;
      ep->ents[ep->n].data = ev->data;
      ep->ents[ep->n].oneshot_armed = 0;
      ep->ents[ep->n].edge_ready = 0;
      ++ep->n;
      return 0;
    case EPOLL_CTL_MOD:
      if (idx == -1) {
        errno = ENOENT;
        return -1;
      }
      if (!ev) {
        errno = EINVAL;
        return -1;
      }
      ep->ents[idx].events = ev->events | EPOLLERR | EPOLLHUP;
      ep->ents[idx].data = ev->data;
      ep->ents[idx].oneshot_armed = 0;
      ep->ents[idx].edge_ready = 0;
      return 0;
    case EPOLL_CTL_DEL:
      if (idx == -1) {
        errno = ENOENT;
        return -1;
      }
      ep->ents[idx] = ep->ents[--ep->n];
      return 0;
    default:
      errno = EINVAL;
      return -1;
  }
}

static uint32_t EpollReadyEvents(const struct WboxEpollEnt *ent,
                                 int16_t revents) {
  uint32_t ready = revents & (POLLIN | POLLOUT | POLLPRI | POLLERR | POLLHUP);
  if (revents & POLLHUP) ready |= EPOLLRDHUP;
  return ready & ent->events;
}

void WboxEpollRefreshFd(int fd) {
  int saved_errno;
  int16_t interests;
  struct pollfd pfd;
  saved_errno = errno;
  interests = 0;
  for (int i = 0; i < WBOX_MAXEPOLL; ++i) {
    struct WboxEpoll *ep = &g_epolls[i];
    if (!ep->used) continue;
    for (int j = 0; j < ep->n; ++j) {
      if (ep->ents[j].fd == fd && (ep->ents[j].events & EPOLLET)) {
        interests |= ep->ents[j].events & (EPOLLIN | EPOLLOUT | EPOLLPRI);
      }
    }
  }
  if (!interests) {
    errno = saved_errno;
    return;
  }
  pfd.fd = fd;
  pfd.events = interests;
  pfd.revents = 0;
  if (W32WaitFds(&pfd, 1, 0) >= 0) {
    for (int i = 0; i < WBOX_MAXEPOLL; ++i) {
      struct WboxEpoll *ep = &g_epolls[i];
      if (!ep->used) continue;
      for (int j = 0; j < ep->n; ++j) {
        struct WboxEpollEnt *ent = &ep->ents[j];
        if (ent->fd == fd && (ent->events & EPOLLET)) {
          ent->edge_ready &= EpollReadyEvents(ent, pfd.revents);
        }
      }
    }
  }
  errno = saved_errno;
}

int epoll_wait(int epfd, struct epoll_event *events, int maxevents,
               int timeout) {
  if (!WboxEpollIsFd(epfd)) {
    errno = EBADF;
    return -1;
  }
  if (maxevents <= 0) {
    errno = EINVAL;
    return -1;
  }
  struct WboxEpoll *ep = EpollFromFd(epfd);
  if (!ep->n) {
    if (timeout > 0) Sleep(timeout);
    return 0;
  }
  struct pollfd *pfds = malloc(ep->n * sizeof(*pfds));
  if (!pfds) {
    errno = ENOMEM;
    return -1;
  }
  DWORD started = GetTickCount();
  for (;;) {
    for (int i = 0; i < ep->n; ++i) {
      pfds[i].fd = ((ep->ents[i].events & EPOLLONESHOT) &&
                    ep->ents[i].oneshot_armed)
                       ? -1
                       : ep->ents[i].fd;
      // EPOLLIN/OUT/PRI low bits coincide with POLLIN/OUT/PRI.
      pfds[i].events =
          (int16_t)(ep->ents[i].events & (EPOLLIN | EPOLLOUT | EPOLLPRI));
      pfds[i].revents = 0;
    }
    int wait = timeout;
    if (timeout > 0) {
      DWORD elapsed = GetTickCount() - started;
      if (elapsed >= (DWORD)timeout) {
        free(pfds);
        return 0;
      }
      wait = timeout - elapsed;
    }
    int rc = W32WaitFds(pfds, ep->n, wait);
    if (rc <= 0) {
      free(pfds);
      return rc < 0 ? -1 : 0;
    }
    int out = 0;
    for (int i = 0; i < ep->n; ++i) {
      struct WboxEpollEnt *ent = &ep->ents[i];
      uint32_t ready = EpollReadyEvents(ent, pfds[i].revents);
      if (ent->events & EPOLLET) {
        uint32_t edge = ready & ~ent->edge_ready;
        if (!edge) {
          ent->edge_ready = ready;
          continue;
        }
        if (out >= maxevents) continue;
        ent->edge_ready = ready;
      } else {
        if (!ready || out >= maxevents) continue;
      }
      events[out].events = ready;
      events[out].data = ent->data;
      if (ent->events & EPOLLONESHOT) ent->oneshot_armed = 1;
      ++out;
    }
    if (out || timeout == 0) {
      free(pfds);
      return out;
    }
    // A latched ET fd remains poll-readable. Avoid returning early or
    // spinning while waiting for it to leave and re-enter the ready state.
    Sleep(1);
  }
}

int epoll_pwait(int epfd, struct epoll_event *events, int maxevents,
                int timeout, const sigset_t *mask) {
  (void)mask;  // guest signal mask is emulated inside blink
  return epoll_wait(epfd, events, maxevents, timeout);
}

// ---------------------------------------------------------------- resolver
// Host-passthrough getaddrinfo/getnameinfo with ABI translation between the
// compat (Linux-layout) addrinfo and the winsock one.

int h_errno;

// winsock returns WSATRY_AGAIN/WSAHOST_NOT_FOUND/... positive codes;
// table centralized in w32errno.c
static int GaiErr(int wsrc) {
  return W32GaiErrFromWsa(wsrc);
}

int getaddrinfo(const char *node, const char *service,
                const struct addrinfo *hints, struct addrinfo **res) {
  if (WsaInit() < 0) return EAI_FAIL;
  struct ws_addrinfo whints, *wres = NULL;
  memset(&whints, 0, sizeof(whints));
  if (hints) {
    whints.ai_flags = hints->ai_flags;
    whints.ai_family = FamToWs(hints->ai_family);
    whints.ai_socktype = hints->ai_socktype;
    whints.ai_protocol = hints->ai_protocol;
  }
  int rc = ws.getaddrinfo(node, service, hints ? &whints : NULL, &wres);
  if (rc != 0) return GaiErr(rc);
  struct addrinfo *head = NULL, **tail = &head;
  for (struct ws_addrinfo *w = wres; w; w = w->ai_next) {
    struct addrinfo *a = calloc(1, sizeof(*a));
    if (!a) break;
    a->ai_flags = w->ai_flags;
    a->ai_family = FamFromWs(w->ai_family);
    a->ai_socktype = w->ai_socktype;
    a->ai_protocol = w->ai_protocol;
    a->ai_addrlen = (socklen_t)w->ai_addrlen;
    if (w->ai_addr) {
      /* analyzer: allocating sockaddr_storage (128B) into a sockaddr* is
         intentional — callers may copy up to ai_addrlen bytes. */
      a->ai_addr = malloc(sizeof(struct sockaddr_storage));
      memset(a->ai_addr, 0, sizeof(struct sockaddr_storage));
      size_t c = w->ai_addrlen < sizeof(struct sockaddr_storage)
                     ? w->ai_addrlen
                     : sizeof(struct sockaddr_storage);
      memcpy(a->ai_addr, w->ai_addr, c);
      a->ai_addr->sa_family = (sa_family_t)a->ai_family;
    }
    if (w->ai_canonname) a->ai_canonname = strdup(w->ai_canonname);
    *tail = a;
    tail = &a->ai_next;
  }
  ws.freeaddrinfo(wres);
  if (!head) return EAI_FAIL;
  *res = head;
  return 0;
}

void freeaddrinfo(struct addrinfo *ai) {
  while (ai) {
    struct addrinfo *next = ai->ai_next;
    free(ai->ai_addr);
    free(ai->ai_canonname);
    free(ai);
    ai = next;
  }
}

int getnameinfo(const struct sockaddr *sa, socklen_t salen, char *host,
                socklen_t hostlen, char *serv, socklen_t servlen, int flags) {
  if (WsaInit() < 0) return EAI_FAIL;
  struct wsockaddr_storage wa;
  if (salen > (socklen_t)sizeof(wa)) salen = sizeof(wa);
  memcpy(&wa, sa, salen);
  wa.ss_family = (uint16_t)FamToWs(wa.ss_family);
  // Linux NI_* values differ from winsock's; translate
  int wf = 0;
  if (flags & 1) wf |= 0x02;   // NI_NUMERICHOST
  if (flags & 2) wf |= 0x08;   // NI_NUMERICSERV
  if (flags & 4) wf |= 0x01;   // NI_NOFQDN
  if (flags & 8) wf |= 0x04;   // NI_NAMEREQD
  if (flags & 16) wf |= 0x10;  // NI_DGRAM
  int rc = ws.getnameinfo((struct wsockaddr *)&wa, salen, host, hostlen,
                          serv, servlen, wf);
  return rc ? GaiErr(rc) : 0;
}

const char *gai_strerror(int err) {
  switch (err) {
    case 0: return "success";
    case EAI_AGAIN: return "temporary failure in name resolution";
    case EAI_NONAME: return "name or service not known";
    case EAI_FAIL: return "non-recoverable failure in name resolution";
    default: return "unknown error";
  }
}

struct hostent *gethostbyname(const char *name) {
  static struct hostent he;
  static char *addrs[2];
  static char addrbuf[16];
  static char *aliases[1];
  if (WsaInit() < 0) return NULL;
  struct ws_hostent *w = ws.gethostbyname(name);
  if (!w) {
    h_errno = ws.WSAGetLastError();
    return NULL;
  }
  memset(&he, 0, sizeof(he));
  he.h_name = w->h_name;
  aliases[0] = NULL;
  he.h_aliases = aliases;
  he.h_addrtype = FamFromWs(w->h_addrtype);
  he.h_length = w->h_length;
  if (w->h_addr_list && w->h_addr_list[0] && w->h_length <= (int)sizeof(addrbuf)) {
    memcpy(addrbuf, w->h_addr_list[0], w->h_length);
    addrs[0] = addrbuf;
    addrs[1] = NULL;
    he.h_addr_list = addrs;
  } else {
    he.h_addr_list = NULL;
  }
  return &he;
}

struct hostent *gethostbyaddr(const void *addr, socklen_t len, int type) {
  (void)addr;
  (void)len;
  (void)type;
  h_errno = 11004;  // WSANO_DATA
  return NULL;
}

// ---------------------------------------------------------------- inet_pton/ntop

static int InetPton4(const char *src, void *dst) {
  unsigned parts[4];
  int n = 0;
  const char *p = src;
  for (int i = 0; i < 4; ++i) {
    if (!*p) return 0;
    unsigned v = 0;
    int digits = 0;
    while (*p >= '0' && *p <= '9') {
      v = v * 10 + (*p - '0');
      if (v > 255) return 0;
      ++digits;
      ++p;
    }
    if (!digits) return 0;
    parts[i] = v;
    ++n;
    if (i < 3) {
      if (*p != '.') return 0;
      ++p;
    }
  }
  if (*p || n != 4) return 0;
  unsigned char *d = dst;
  for (int i = 0; i < 4; ++i) d[i] = (unsigned char)parts[i];
  return 1;
}

int inet_pton(int af, const char *src, void *dst) {
  if (WsaInit() < 0) {
    errno = EAFNOSUPPORT;
    return -1;
  }
  if (af == AF_INET && !ws.InetPtonA) return InetPton4(src, dst);
  if (!ws.InetPtonA) {
    errno = EAFNOSUPPORT;
    return -1;
  }
  int waf = FamToWs(af);
  if (waf != WS_AF_INET && waf != WS_AF_INET6) {
    errno = EAFNOSUPPORT;
    return -1;
  }
  return ws.InetPtonA(waf, src, dst);
}

const char *inet_ntop(int af, const void *src, char *dst, socklen_t size) {
  if (WsaInit() < 0 || !ws.InetNtopA) {
    if (af == AF_INET) {
      const unsigned char *d = src;
      snprintf(dst, size, "%u.%u.%u.%u", d[0], d[1], d[2], d[3]);
      return dst;
    }
    errno = EAFNOSUPPORT;
    return NULL;
  }
  int waf = FamToWs(af);
  if (waf != WS_AF_INET && waf != WS_AF_INET6) {
    errno = EAFNOSUPPORT;
    return NULL;
  }
  // winsock InetNtopA takes a non-const void* on some SDKs
  return ws.InetNtopA(waf, (void *)src, dst, size);
}

// ---------------------------------------------------------------- termios
// Console-mode tracking with real canonical/echo/isig mapping so
// bash/readline get a usable raw mode. (isatty lives in w32fd.c;
// TIOCGWINSZ lives in w32fd.c ioctl.)

static struct termios g_tios = {
    .c_iflag = ICRNL | IXON,
    .c_oflag = OPOST | ONLCR,
    .c_cflag = CS8 | CREAD | HUPCL,
    .c_lflag = ISIG | ICANON | ECHO | ECHOE | ECHOK | IEXTEN,
    .c_line = 0,
    .c_cc = {3, 28, 127, 21, 4, 0, 1, 0, 17, 19, 26, 0, 18, 15, 23, 22, 0},
    .c_ispeed = B38400,
    .c_ospeed = B38400,
};

static void WboxApplyTermios(int fd, const struct termios *t) {
  HANDLE h = (HANDLE)_get_osfhandle(fd);
  if (h == INVALID_HANDLE_VALUE) return;
  DWORD m;
  if (!GetConsoleMode(h, &m)) return;  // not a console (pipe/file/socket)
  if (t->c_lflag & ICANON) {
    m |= ENABLE_LINE_INPUT;
  } else {
    m &= ~ENABLE_LINE_INPUT;
    m |= ENABLE_VIRTUAL_TERMINAL_INPUT;
  }
  if (t->c_lflag & ECHO) {
    m |= ENABLE_ECHO_INPUT;
  } else {
    m &= ~ENABLE_ECHO_INPUT;
  }
  if (t->c_lflag & ISIG) {
    m |= ENABLE_PROCESSED_INPUT;
  } else {
    m &= ~ENABLE_PROCESSED_INPUT;
  }
  m |= ENABLE_EXTENDED_FLAGS | ENABLE_WINDOW_INPUT;
  SetConsoleMode(h, m);
  // output side: VT processing for readline/colors when OPOST is on
  HANDLE ho = GetStdHandle(STD_OUTPUT_HANDLE);
  DWORD om;
  if (GetConsoleMode(ho, &om)) {
    om |= ENABLE_PROCESSED_OUTPUT;
    if (t->c_oflag & OPOST) om |= ENABLE_VIRTUAL_TERMINAL_PROCESSING;
    SetConsoleMode(ho, om);
  }
}

int tcgetattr(int fd, struct termios *t) {
  if (!isatty(fd)) {
    errno = ENOTTY;
    return -1;
  }
  *t = g_tios;
  return 0;
}

int tcsetattr(int fd, int act, const struct termios *t) {
  (void)act;  // drain/flush are no-ops on the console
  if (!isatty(fd)) {
    errno = ENOTTY;
    return -1;
  }
  g_tios = *t;
  WboxApplyTermios(fd, t);
  return 0;
}
