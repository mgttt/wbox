#ifndef WBOX_COMPAT_NETINET_IN_H
#define WBOX_COMPAT_NETINET_IN_H
#include <stdint.h>
#include <sys/socket.h>
typedef uint32_t in_addr_t;
typedef uint16_t in_port_t;
struct in_addr { in_addr_t s_addr; };
struct in6_addr { uint8_t s6_addr[16]; };
struct sockaddr_in {
  sa_family_t sin_family;
  in_port_t sin_port;
  struct in_addr sin_addr;
  uint8_t sin_zero[8];
};
struct sockaddr_in6 {
  sa_family_t sin6_family;
  in_port_t sin6_port;
  uint32_t sin6_flowinfo;
  struct in6_addr sin6_addr;
  uint32_t sin6_scope_id;
};
#define INADDR_ANY 0
#define INADDR_BROADCAST 0xffffffffu
#define INADDR_LOOPBACK 0x7f000001u
#define INADDR_NONE 0xffffffffu
#define IPPROTO_IP 0
#define IPPROTO_TCP 6
#define IPPROTO_UDP 17
#define IPPROTO_IPV6 41
#define IPPROTO_ICMP 1
#define IPPROTO_RAW 255
#define IP_TTL 2
#define IP_MULTICAST_TTL 33
#define IP_MULTICAST_LOOP 34
#define IP_ADD_MEMBERSHIP 35
#define IP_DROP_MEMBERSHIP 36
#define IPV6_UNICAST_HOPS 16
#define IPV6_MULTICAST_HOPS 18
#define IPV6_MULTICAST_LOOP 19
#define IPV6_V6ONLY 26
#define IPV6_ADD_MEMBERSHIP 20
#define IPV6_DROP_MEMBERSHIP 21
extern const struct in6_addr in6addr_any;
extern const struct in6_addr in6addr_loopback;
#endif
