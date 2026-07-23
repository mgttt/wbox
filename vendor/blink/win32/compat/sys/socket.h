#ifndef WBOX_COMPAT_SYS_SOCKET_H
#define WBOX_COMPAT_SYS_SOCKET_H
#include <sys/types.h>
#include <stdint.h>

typedef uint32_t socklen_t;
typedef uint16_t sa_family_t;

struct sockaddr {
  sa_family_t sa_family;
  char sa_data[14];
};

struct sockaddr_storage {
  sa_family_t ss_family;
  char __pad[126];
};

#define AF_UNSPEC 0
#define AF_UNIX 1
#define AF_LOCAL AF_UNIX
#define AF_INET 2
#define AF_INET6 10
#define AF_NETLINK 16
#define AF_PACKET 17
#define PF_UNIX AF_UNIX
#define PF_INET AF_INET
#define PF_INET6 AF_INET6

#define SOCK_STREAM 1
#define SOCK_DGRAM 2
#define SOCK_RAW 3
#define SOCK_SEQPACKET 5
#define SOCK_NONBLOCK 0x800
#define SOCK_CLOEXEC 0x80000

#define SOL_SOCKET 1
#define SO_REUSEADDR 2
#define SO_TYPE 3
#define SO_ERROR 4
#define SO_BROADCAST 6
#define SO_SNDBUF 7
#define SO_RCVBUF 8
#define SO_KEEPALIVE 9
#define SO_OOBINLINE 10
#define SO_LINGER 13
#define SO_RCVTIMEO 20
#define SO_SNDTIMEO 21
#define SO_ACCEPTCONN 30

#define MSG_OOB 1
#define MSG_PEEK 2
#define MSG_DONTROUTE 4
#define MSG_CTRUNC 8
#define MSG_TRUNC 0x20
#define MSG_DONTWAIT 0x40
#define MSG_EOR 0x80
#define MSG_WAITALL 0x100
#define MSG_NOSIGNAL 0x4000
#define MSG_CMSG_CLOEXEC 0x40000000

#define SHUT_RD 0
#define SHUT_WR 1
#define SHUT_RDWR 2

struct msghdr {
  void *msg_name;
  socklen_t msg_namelen;
  struct iovec *msg_iov;
  size_t msg_iovlen;
  void *msg_control;
  socklen_t msg_controllen;
  int msg_flags;
};

struct cmsghdr {
  size_t cmsg_len;
  int cmsg_level;
  int cmsg_type;
};

#define SCM_RIGHTS 1
#define SCM_CREDENTIALS 2

#define CMSG_ALIGN(len) (((len) + sizeof(size_t) - 1) & ~(sizeof(size_t) - 1))
#define CMSG_LEN(len) (CMSG_ALIGN(sizeof(struct cmsghdr)) + (len))
#define CMSG_SPACE(len) (CMSG_ALIGN(sizeof(struct cmsghdr)) + CMSG_ALIGN(len))
#define CMSG_DATA(cmsg) ((unsigned char *)(cmsg) + CMSG_ALIGN(sizeof(struct cmsghdr)))
#define CMSG_FIRSTHDR(mhdr) ((mhdr)->msg_controllen >= sizeof(struct cmsghdr) ? (struct cmsghdr *)(mhdr)->msg_control : (struct cmsghdr *)0)
#define CMSG_NXTHDR(mhdr, cmsg) ((struct cmsghdr *)0)

struct linger {
  int l_onoff;
  int l_linger;
};

#ifdef __cplusplus
extern "C" {
#endif
int socket(int, int, int);
int socketpair(int, int, int, int[2]);
int bind(int, const struct sockaddr *, socklen_t);
int connect(int, const struct sockaddr *, socklen_t);
int listen(int, int);
int accept(int, struct sockaddr *, socklen_t *);
int accept4(int, struct sockaddr *, socklen_t *, int);
int shutdown(int, int);
ssize_t send(int, const void *, size_t, int);
ssize_t recv(int, void *, size_t, int);
ssize_t sendto(int, const void *, size_t, int, const struct sockaddr *, socklen_t);
ssize_t recvfrom(int, void *, size_t, int, struct sockaddr *, socklen_t *);
ssize_t sendmsg(int, const struct msghdr *, int);
ssize_t recvmsg(int, struct msghdr *, int);
int getsockopt(int, int, int, void *, socklen_t *);
int setsockopt(int, int, int, const void *, socklen_t);
int getsockname(int, struct sockaddr *, socklen_t *);
int getpeername(int, struct sockaddr *, socklen_t *);
int sockatmark(int);
#ifdef __cplusplus
}
#endif
#endif
