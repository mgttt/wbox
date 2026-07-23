#ifndef BLINK_CONFIG_H_
#define BLINK_CONFIG_H_
/* wbox win32 L1 bring-up config */
#define DISABLE_JIT
#define DISABLE_SOCKETS
#define DISABLE_VFS
#define DISABLE_OVERLAYS
#define DISABLE_ANCILLARY
#define DISABLE_DISASSEMBLER
#define DISABLE_BACKTRACE
#define DISABLE_LIBUNWIND
#define DISABLE_METAL
#define HAVE_INT128
/* suppress CRT struct stat so our compat version wins */
#define _STAT_DEFINED 1
#define _INC_STAT 1
#define _INC_STAT_INL 1
#endif

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef _WINSOCKAPI_
#define _WINSOCKAPI_
#endif
#include <windows.h>
/* use 32-bit pids like Linux */
#define _PID_T_ 1
typedef int pid_t;
