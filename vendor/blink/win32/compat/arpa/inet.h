#ifndef WBOX_COMPAT_ARPA_INET_H
#define WBOX_COMPAT_ARPA_INET_H
#include <stdint.h>
#include <netinet/in.h>
uint32_t htonl(uint32_t);
uint16_t htons(uint16_t);
uint32_t ntohl(uint32_t);
uint16_t ntohs(uint16_t);
in_addr_t inet_addr(const char *);
int inet_pton(int, const char *, void *);
const char *inet_ntop(int, const void *, char *, socklen_t);
#define INET_ADDRSTRLEN 16
#define INET6_ADDRSTRLEN 46
#endif
