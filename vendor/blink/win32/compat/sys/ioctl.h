#ifndef WBOX_COMPAT_SYS_IOCTL_H
#define WBOX_COMPAT_SYS_IOCTL_H
#include <sys/types.h>
#define TIOCGWINSZ 0x5413
#define TIOCSWINSZ 0x5414
#define TIOCGPGRP 0x540F
#define TIOCSPGRP 0x5410
#define TIOCSCTTY 0x540E
#define TIOCNOTTY 0x5422
#define TCGETS 0x5401
#define TCSETS 0x5402
#define TCSETSW 0x5403
#define TCSETSF 0x5404
#define TIOCGSID 0x5429
#define FIONREAD 0x541B
#define FIONBIO 0x5421
#define FIOASYNC 0x5452
#define TIOCEXCL 0x540C
#define TIOCNXCL 0x540D
#define TIOCSTI 0x5412
#define TIOCMGET 0x5815
#define TIOCMSET 0x5818
#define KDGKBMODE 0x4B44
#define KDSETMODE 0x4B3A
#define VT_SETMODE 0x5602
#define VT_GETMODE 0x5601
#define VT_OPENQRY 0x5600
#define SIOCGIFCONF 0x8912
#define SIOCGIFFLAGS 0x8913
#define SIOCGIFADDR 0x8915
#define SIOCGIFNETMASK 0x891b
#define SIOCGIFBRDADDR 0x8919
struct winsize { unsigned short ws_row, ws_col, ws_xpixel, ws_ypixel; };
int ioctl(int, unsigned long, ...);
#endif
