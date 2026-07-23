#ifndef WBOX_COMPAT_STDIO_H
#define WBOX_COMPAT_STDIO_H
#include_next <stdio.h>
int renameat(int, const char *, int, const char *);
int renameat2(int, const char *, int, const char *, unsigned);
int dprintf(int, const char *, ...);
int vdprintf(int, const char *, va_list);
int asprintf(char **, const char *, ...);
int vasprintf(char **, const char *, va_list);
char *tmpnam_r(char *);
#endif
