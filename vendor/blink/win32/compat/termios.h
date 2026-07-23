#ifndef WBOX_COMPAT_TERMIOS_H
#define WBOX_COMPAT_TERMIOS_H
#include <sys/types.h>
typedef unsigned int tcflag_t;
typedef unsigned char cc_t;
typedef unsigned int speed_t;
#define NCCS 32
struct termios {
  tcflag_t c_iflag, c_oflag, c_cflag, c_lflag;
  cc_t c_line;
  cc_t c_cc[NCCS];
  speed_t c_ispeed, c_ospeed;
};
#define VINTR 0
#define VQUIT 1
#define VERASE 2
#define VKILL 3
#define VEOF 4
#define VTIME 5
#define VMIN 6
#define VSWTC 7
#define VSTART 8
#define VSTOP 9
#define VSUSP 10
#define VEOL 11
#define VREPRINT 12
#define VDISCARD 13
#define VWERASE 14
#define VLNEXT 15
#define VEOL2 16
#define IGNBRK 0x1
#define BRKINT 0x2
#define IGNPAR 0x4
#define PARMRK 0x8
#define INPCK 0x10
#define ISTRIP 0x20
#define INLCR 0x40
#define IGNCR 0x80
#define ICRNL 0x100
#define IUCLC 0x200
#define IXON 0x400
#define IXANY 0x800
#define IXOFF 0x1000
#define IMAXBEL 0x2000
#define IUTF8 0x4000
#define OPOST 0x1
#define OLCUC 0x2
#define ONLCR 0x4
#define OCRNL 0x8
#define ONOCR 0x10
#define ONLRET 0x20
#define OFILL 0x40
#define OFDEL 0x80
#define NLDLY 0x100
#define NL0 0
#define NL1 0x100
#define CRDLY 0x600
#define CR0 0
#define CR1 0x200
#define CR2 0x400
#define CR3 0x600
#define TABDLY 0x1800
#define TAB0 0
#define TAB1 0x800
#define TAB2 0x1000
#define TAB3 0x1800
#define BSDLY 0x2000
#define BS0 0
#define BS1 0x2000
#define VTDLY 0x4000
#define VT0 0
#define VT1 0x4000
#define FFDLY 0x8000
#define FF0 0
#define FF1 0x8000
#define XCASE 0x4
#define CBAUD 0x100f
#define CBAUDEX 0x1000
#define CIBAUD 0x100f0000
#define CMSPAR 0x40000000
#define CRTSCTS 0x80000000
#define CSIZE 0x30
#define CS5 0
#define CS6 0x10
#define CS7 0x20
#define CS8 0x30
#define CSTOPB 0x40
#define CREAD 0x80
#define PARENB 0x100
#define PARODD 0x200
#define HUPCL 0x400
#define CLOCAL 0x800
#define ISIG 0x1
#define ICANON 0x2
#define ECHO 0x8
#define ECHOE 0x10
#define ECHOK 0x20
#define ECHONL 0x40
#define NOFLSH 0x80
#define TOSTOP 0x100
#define IEXTEN 0x8000
#define ECHOCTL 0x200
#define ECHOPRT 0x400
#define ECHOKE 0x800
#define PENDIN 0x4000
#define B0 0
#define B50 1
#define B75 2
#define B110 3
#define B134 4
#define B150 5
#define B200 6
#define B300 7
#define B600 8
#define B1200 9
#define B1800 10
#define B2400 11
#define B4800 12
#define B9600 13
#define B19200 14
#define B38400 15
#define B57600 0x1001
#define B115200 0x1002
#define TCSANOW 0
#define TCSADRAIN 1
#define TCSAFLUSH 2
#define TCOOFF 0
#define TCOON 1
#define TCIOFF 2
#define TCION 3
#define TCIFLUSH 0
#define TCOFLUSH 1
#define TCIOFLUSH 2
int tcgetattr(int, struct termios *);
int tcsetattr(int, int, const struct termios *);
int tcdrain(int);
int tcflow(int, int);
int tcflush(int, int);
int tcsendbreak(int, int);
speed_t cfgetispeed(const struct termios *);
speed_t cfgetospeed(const struct termios *);
int cfsetispeed(struct termios *, speed_t);
int cfsetospeed(struct termios *, speed_t);
void cfmakeraw(struct termios *);
pid_t tcgetsid(int);
#endif
