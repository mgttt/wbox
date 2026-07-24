#ifndef WBOX_COMPAT_STDLIB_H
#define WBOX_COMPAT_STDLIB_H
#include_next <stdlib.h>
int posix_memalign(void **, size_t, size_t);
void *memalign(size_t, size_t);
void *aligned_alloc(size_t, size_t);
void *valloc(size_t);
int mkostemp(char *, int);
int clearenv(void);
int setenv(const char *, const char *, int);
int unsetenv(const char *);
char *realpath(const char *, char *);
#endif
