#ifndef WBOX_COMPAT_SYS_MMAN_H
#define WBOX_COMPAT_SYS_MMAN_H
#include <sys/types.h>
#include <stddef.h>

#define PROT_NONE  0
#define PROT_READ  1
#define PROT_WRITE 2
#define PROT_EXEC  4

#define MAP_SHARED    0x01
#define MAP_PRIVATE   0x02
#define MAP_FIXED     0x10
#define MAP_ANONYMOUS 0x20
#define MAP_ANON      MAP_ANONYMOUS
#define MAP_NORESERVE 0x4000
#define MAP_FIXED_NOREPLACE 0x100000
#define MAP_POPULATE  0x8000
#define MAP_NONBLOCK  0x10000
#define MAP_STACK     0x20000
#define MAP_HUGETLB   0x40000
#define MAP_GROWSDOWN 0x100
#define MAP_DENYWRITE 0x800
#define MAP_EXECUTABLE 0x1000
#define MAP_LOCKED    0x2000

#define MS_ASYNC      1
#define MS_INVALIDATE 2
#define MS_SYNC       4

#define MADV_NORMAL      0
#define MADV_RANDOM      1
#define MADV_SEQUENTIAL  2
#define MADV_WILLNEED    3
#define MADV_DONTNEED    4
#define MADV_FREE        8

#define MAP_FAILED ((void *)-1)

#ifdef __cplusplus
extern "C" {
#endif
void *mmap(void *, size_t, int, int, int, off_t);
int munmap(void *, size_t);
int mprotect(void *, size_t, int);
int msync(void *, size_t, int);
int madvise(void *, size_t, int);
int mlock(const void *, size_t);
int munlock(const void *, size_t);
void *mremap(void *, size_t, size_t, int, ...);
#ifdef __cplusplus
}
#endif
#define MREMAP_MAYMOVE 1
#define MREMAP_FIXED 2
#endif
