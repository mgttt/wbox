#ifndef WBOX_COMPAT_STRING_H
#define WBOX_COMPAT_STRING_H
#include_next <string.h>
char *stpcpy(char *, const char *);
char *stpncpy(char *, const char *, size_t);
char *strchrnul(const char *, int);
void *memccpy(void *, const void *, int, size_t);
void *mempcpy(void *, const void *, size_t);
void *rawmemchr(const void *, int);
void *memrchr(const void *, int, size_t);
int strverscmp(const char *, const char *);
void explicit_bzero(void *, size_t);
#endif
