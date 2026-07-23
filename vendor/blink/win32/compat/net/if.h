#ifndef WBOX_COMPAT_NET_IF_H
#define WBOX_COMPAT_NET_IF_H
#include <sys/socket.h>
#define IFNAMSIZ 16
#define IFF_UP 0x1
#define IFF_BROADCAST 0x2
#define IFF_LOOPBACK 0x8
#define IFF_RUNNING 0x40
struct ifreq {
  char ifr_name[IFNAMSIZ];
  union {
    struct sockaddr ifr_addr;
    struct sockaddr ifr_dstaddr;
    struct sockaddr ifr_broadaddr;
    struct sockaddr ifr_netmask;
    short ifr_flags;
    int ifr_metric;
    int ifr_mtu;
  };
};
struct ifconf { int ifc_len; union { char *ifc_buf; struct ifreq *ifc_req; }; };
unsigned int if_nametoindex(const char *);
#endif
