/* wtest.h — minimal guest-side assert framework for wbox-linux regression tests.
 *
 * Each test file is a standalone static musl binary (own main()).
 * Usage:
 *     #include "wtest.h"
 *     int main(void) {
 *         T_BEGIN("mmap/anon-private");
 *         ...
 *         T_ASSERT(p != MAP_FAILED);
 *         return WTEST_END();
 *     }
 *
 * Output lines:  PASS <group>  /  FAIL <group>: file:line: expr  /
 *                SKIP <group> — reason
 * and a final    SUMMARY pass=N fail=M skip=K
 * Exit code: 0 when no FAIL (skips allowed), 1 otherwise.
 */
#ifndef WTEST_H
#define WTEST_H

#include <stdio.h>
#include <string.h>
#include <errno.h>

static int wtest_pass = 0, wtest_fail = 0, wtest_skip = 0;
static const char *wtest_cur = "(init)";

#define T_BEGIN(name) do { wtest_cur = (name); } while (0)

#define T_ASSERT(cond) do {                                            \
    if (cond) {                                                        \
        wtest_pass++;                                                  \
        printf("PASS %s\n", wtest_cur);                                \
    } else {                                                           \
        wtest_fail++;                                                  \
        printf("FAIL %s: %s:%d: %s\n", wtest_cur, __FILE__, __LINE__,  \
               #cond);                                                 \
    }                                                                  \
} while (0)

#define T_ASSERT_EQ(a, b) do {                                         \
    long _va = (long)(a), _vb = (long)(b);                             \
    if (_va == _vb) {                                                  \
        wtest_pass++;                                                  \
        printf("PASS %s\n", wtest_cur);                                \
    } else {                                                           \
        wtest_fail++;                                                  \
        printf("FAIL %s: %s:%d: %s(=%ld) != %s(=%ld)\n", wtest_cur,    \
               __FILE__, __LINE__, #a, _va, #b, _vb);                  \
    }                                                                  \
} while (0)

/* syscall-ish expr must return -1 with errno == e */
#define T_ASSERT_ERRNO(expr, e) do {                                   \
    errno = 0;                                                         \
    long _r = (long)(expr);                                            \
    int _e = errno;                                                    \
    if (_r == -1 && _e == (e)) {                                       \
        wtest_pass++;                                                  \
        printf("PASS %s\n", wtest_cur);                                \
    } else {                                                           \
        wtest_fail++;                                                  \
        printf("FAIL %s: %s:%d: %s => ret=%ld errno=%d (%s), want "    \
               "ret=-1 errno=%d (%s)\n", wtest_cur, __FILE__, __LINE__,\
               #expr, _r, _e, strerror(_e), (e), strerror(e));         \
    }                                                                  \
} while (0)

/* expr must succeed (ret != -1) */
#define T_ASSERT_OK(expr) do {                                         \
    errno = 0;                                                         \
    long _r = (long)(expr);                                            \
    int _e = errno;                                                    \
    if (_r != -1) {                                                    \
        wtest_pass++;                                                  \
        printf("PASS %s\n", wtest_cur);                                \
    } else {                                                           \
        wtest_fail++;                                                  \
        printf("FAIL %s: %s:%d: %s => -1 errno=%d (%s)\n", wtest_cur,  \
               __FILE__, __LINE__, #expr, _e, strerror(_e));           \
    }                                                                  \
} while (0)

#define T_SKIP(name, why) do {                                         \
    wtest_skip++;                                                      \
    printf("SKIP %s — %s\n", (name), (why));                           \
} while (0)

#define WTEST_END() (                                                  \
    printf("SUMMARY pass=%d fail=%d skip=%d\n",                        \
           wtest_pass, wtest_fail, wtest_skip),                        \
    wtest_fail != 0)

#endif /* WTEST_H */
