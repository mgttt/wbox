#ifndef WBOX_COMPAT_SYS_SYSINFO_H
#define WBOX_COMPAT_SYS_SYSINFO_H
#include <sys/types.h>
struct sysinfo {
  long uptime;
  unsigned long loads[3];
  unsigned long totalram, freeram, sharedram, bufferram;
  unsigned long totalswap, freeswap;
  unsigned short procs, pad;
  unsigned long totalhigh, freehigh;
  unsigned mem_unit;
  char _f[0];
};
int sysinfo(struct sysinfo *);
#endif
