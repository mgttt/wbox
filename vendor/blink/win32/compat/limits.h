#ifndef WBOX_COMPAT_LIMITS_H
#define WBOX_COMPAT_LIMITS_H
#include_next <limits.h>
#undef PATH_MAX
#define PATH_MAX 4096
#ifndef PIPE_BUF
#define PIPE_BUF 4096
#endif
#ifndef NAME_MAX
#define NAME_MAX 255
#endif
#define ARG_MAX 131072
#define HOST_NAME_MAX 64
/* UCRT <limits.h> 不定义 SSIZE_MAX（POSIX 扩展）。mingw 的 ssize_t 为
   指针宽度，故以 INTPTR_MAX 兜底（x86_64 下即 2^63-1）。 */
#include <stdint.h>
#ifndef SSIZE_MAX
#define SSIZE_MAX INTPTR_MAX
#endif
#endif
