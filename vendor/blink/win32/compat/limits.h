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
#endif
