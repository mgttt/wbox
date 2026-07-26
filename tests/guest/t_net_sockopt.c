/* t_net_sockopt.c — socket option matrix, nonblocking connect EINPROGRESS
 * (d7af906 regression), FIONREAD, poll timeout precision. */
#define _GNU_SOURCE
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/ioctl.h>
#include <sys/poll.h>
#include <sys/time.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <fcntl.h>
#include <unistd.h>
#include <time.h>
#include "wtest.h"

static long now_ms(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

/* AF_UNIX socketpair with TCP-loopback fallback for other hosts. */
static int have_afunix = 1;
static int tcp_pair(int sv[2]) {
  int ls = socket(AF_INET, SOCK_STREAM, 0);
  if (ls < 0) return -1;
  struct sockaddr_in a = {0};
  a.sin_family = AF_INET;
  a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  if (bind(ls, (struct sockaddr *)&a, sizeof a) || listen(ls, 1)) goto err;
  socklen_t l = sizeof a;
  if (getsockname(ls, (struct sockaddr *)&a, &l)) goto err;
  sv[1] = socket(AF_INET, SOCK_STREAM, 0);
  if (sv[1] < 0) goto err;
  if (connect(sv[1], (struct sockaddr *)&a, sizeof a)) { close(sv[1]); goto err; }
  sv[0] = accept(ls, NULL, NULL);
  close(ls);
  if (sv[0] < 0) { close(sv[1]); return -1; }
  return 0;
err:
  close(ls);
  return -1;
}
static int mkpair(int sv[2]) {
  if (have_afunix && socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0) return 0;
  have_afunix = 0;
  return tcp_pair(sv);
}

int main(void) {
  /* --- socket creation matrix --- */
  T_BEGIN("sock/create-types");
  int s = socket(AF_INET, SOCK_STREAM, 0);
  T_ASSERT(s >= 0);
  if (s >= 0) {
    T_ASSERT_ERRNO(lseek(s, 0, SEEK_CUR), ESPIPE);
    T_ASSERT_ERRNO(ftruncate(s, 0), EINVAL);
    T_ASSERT_ERRNO(fsync(s), EINVAL);
    T_ASSERT_ERRNO(fdatasync(s), EINVAL);
    close(s);
  }
  s = socket(AF_INET, SOCK_DGRAM, 0);
  T_ASSERT(s >= 0);
  if (s >= 0) close(s);
  s = socket(AF_UNIX, SOCK_STREAM, 0);
  T_ASSERT(s >= 0);
  if (s >= 0) close(s);

  /* --- AF_UNIX pathname and anonymous-pair semantics --- */
  T_BEGIN("sock/afunix-path");
  {
    const char *path = "t_net_sockopt.sock";
    struct sockaddr_un un = {0};
    int ls = -1, cs = -1, as = -1;
    char ch = 0;
    unlink(path);
    un.sun_family = AF_UNIX;
    strcpy(un.sun_path, path);
    T_ASSERT((ls = socket(AF_UNIX, SOCK_STREAM, 0)) >= 0);
    if (ls >= 0) {
      T_ASSERT_OK(bind(ls, (struct sockaddr *)&un, sizeof(un)));
      T_ASSERT_OK(listen(ls, 1));
      T_ASSERT((cs = socket(AF_UNIX, SOCK_STREAM, 0)) >= 0);
      if (cs >= 0) T_ASSERT_OK(connect(cs, (struct sockaddr *)&un, sizeof(un)));
      T_ASSERT((as = accept(ls, NULL, NULL)) >= 0);
      if (cs >= 0 && as >= 0) {
        T_ASSERT_EQ(write(cs, "u", 1), 1);
        T_ASSERT_EQ(read(as, &ch, 1), 1);
        T_ASSERT_EQ(ch, 'u');
      }
    }
    if (as >= 0) close(as);
    if (cs >= 0) close(cs);
    if (ls >= 0) close(ls);
    T_ASSERT_OK(unlink(path));
  }

  T_BEGIN("sockpair/dgram");
  {
    int sv[2] = {-1, -1};
    char b[4] = {0};
    struct sockaddr_storage ss;
    socklen_t slen = sizeof(ss);
    T_ASSERT_OK(socketpair(AF_UNIX, SOCK_DGRAM, 0, sv));
    if (sv[0] >= 0 && sv[1] >= 0) {
      T_ASSERT_OK(getsockname(sv[0], (struct sockaddr *)&ss, &slen));
      T_ASSERT_EQ(ss.ss_family, AF_UNIX);
      slen = sizeof(ss);
      T_ASSERT_OK(getpeername(sv[0], (struct sockaddr *)&ss, &slen));
      T_ASSERT_EQ(ss.ss_family, AF_UNIX);
      T_ASSERT_EQ(write(sv[0], "ab", 2), 2);
      T_ASSERT_EQ(read(sv[1], b, sizeof(b)), 2);
      T_ASSERT(!memcmp(b, "ab", 2));
      T_ASSERT_EQ(write(sv[1], "z", 1), 1);
      T_ASSERT_EQ(read(sv[0], b, sizeof(b)), 1);
      T_ASSERT_EQ(b[0], 'z');
      close(sv[0]);
      close(sv[1]);
    }
  }

  T_BEGIN("sockpair/flags-errors");
  {
    int sv[2] = {-1, -1};
    T_ASSERT_OK(socketpair(AF_UNIX,
                           SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC,
                           0, sv));
    if (sv[0] >= 0 && sv[1] >= 0) {
      T_ASSERT(fcntl(sv[0], F_GETFL) & O_NONBLOCK);
      T_ASSERT(fcntl(sv[1], F_GETFL) & O_NONBLOCK);
      T_ASSERT(fcntl(sv[0], F_GETFD) & FD_CLOEXEC);
      T_ASSERT(fcntl(sv[1], F_GETFD) & FD_CLOEXEC);
      close(sv[0]);
      close(sv[1]);
    }
    T_ASSERT_ERRNO(socketpair(AF_INET, SOCK_STREAM, 0, sv), EAFNOSUPPORT);
    T_ASSERT_ERRNO(socketpair(AF_UNIX, SOCK_RAW, 0, sv), ESOCKTNOSUPPORT);
  }

  /* --- sockopt get/set matrix on TCP socket --- */
  T_BEGIN("sockopt/reuseaddr");
  s = socket(AF_INET, SOCK_STREAM, 0);
  T_ASSERT(s >= 0);
  int one = 1, got = 0;
  socklen_t len = sizeof got;
  T_ASSERT_OK(setsockopt(s, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one));
  T_ASSERT_OK(getsockopt(s, SOL_SOCKET, SO_REUSEADDR, &got, &len));
  T_ASSERT_EQ(got, 1);
  T_ASSERT_EQ(len, sizeof got);

  T_BEGIN("sockopt/keepalive");
  T_ASSERT_OK(setsockopt(s, SOL_SOCKET, SO_KEEPALIVE, &one, sizeof one));
  got = 0; len = sizeof got;
  T_ASSERT_OK(getsockopt(s, SOL_SOCKET, SO_KEEPALIVE, &got, &len));
  T_ASSERT_EQ(got, 1);

  T_BEGIN("sockopt/tcp-nodelay");
  T_ASSERT_OK(setsockopt(s, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one));
  got = 0; len = sizeof got;
  T_ASSERT_OK(getsockopt(s, IPPROTO_TCP, TCP_NODELAY, &got, &len));
  T_ASSERT_EQ(got, 1);

  T_BEGIN("sockopt/rcvbuf");
  int sz = 65536;
  T_ASSERT_OK(setsockopt(s, SOL_SOCKET, SO_RCVBUF, &sz, sizeof sz));
  got = 0; len = sizeof got;
  T_ASSERT_OK(getsockopt(s, SOL_SOCKET, SO_RCVBUF, &got, &len));
  T_ASSERT(got >= 65536); /* kernel may double */
  close(s);

  T_BEGIN("sockopt/error-clears");
  s = socket(AF_INET, SOCK_STREAM, 0);
  got = 123; len = sizeof got;
  T_ASSERT_OK(getsockopt(s, SOL_SOCKET, SO_ERROR, &got, &len));
  T_ASSERT_EQ(got, 0);
  close(s);

  /* --- nonblocking connect EINPROGRESS exact errno (d7af906) --- */
  T_BEGIN("connect/nonblock-EINPROGRESS");
  s = socket(AF_INET, SOCK_STREAM, 0);
  T_ASSERT(s >= 0);
  int fl = fcntl(s, F_GETFL);
  T_ASSERT_OK(fcntl(s, F_SETFL, fl | O_NONBLOCK));
  struct sockaddr_in sa = {0};
  sa.sin_family = AF_INET;
  sa.sin_port = htons(80);
  /* 10.255.255.1 is non-routable: connect can't complete instantly */
  inet_pton(AF_INET, "10.255.255.1", &sa.sin_addr);
  errno = 0;
  int r = connect(s, (struct sockaddr *)&sa, sizeof sa);
  int e = errno;
  if (r == -1 && e == EINPROGRESS) {
    wtest_pass++; printf("PASS %s\n", wtest_cur);
  } else if (r == -1 && (e == ENETUNREACH || e == EHOSTUNREACH)) {
    T_SKIP("connect/nonblock-EINPROGRESS", "network unreachable in sandbox");
  } else {
    wtest_fail++;
    printf("FAIL %s: connect ret=%d errno=%d (%s), want EINPROGRESS\n",
           wtest_cur, r, e, strerror(e));
  }

  T_BEGIN("connect/second-call-EALREADY");
  if (r == -1 && e == EINPROGRESS) {
    errno = 0;
    int r2 = connect(s, (struct sockaddr *)&sa, sizeof sa);
    T_ASSERT(r2 == -1 && errno == EALREADY);
  } else {
    T_SKIP("connect/second-call-EALREADY", "first connect not in progress");
  }

  /* blocking connect to a live loopback listener must succeed */
  T_BEGIN("connect/loopback-success");
  {
    int ls = socket(AF_INET, SOCK_STREAM, 0);
    T_ASSERT(ls >= 0);
    int one2 = 1;
    setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &one2, sizeof one2);
    struct sockaddr_in la = {0};
    la.sin_family = AF_INET;
    la.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    la.sin_port = 0;
    T_ASSERT_OK(bind(ls, (struct sockaddr *)&la, sizeof la));
    T_ASSERT_OK(listen(ls, 4));
    socklen_t ll = sizeof la;
    T_ASSERT_OK(getsockname(ls, (struct sockaddr *)&la, &ll));
    int cs = socket(AF_INET, SOCK_STREAM, 0);
    T_ASSERT(cs >= 0);
    T_ASSERT_OK(connect(cs, (struct sockaddr *)&la, sizeof la));
    int as = accept(ls, NULL, NULL);
    T_ASSERT(as >= 0);
    /* data flows both ways */
    T_ASSERT_EQ(write(cs, "AB", 2), 2);
    char b[4] = {0};
    T_ASSERT_EQ(read(as, b, 2), 2);
    T_ASSERT_EQ(memcmp(b, "AB", 2), 0);
    close(as); close(cs); close(ls);
  }
  close(s);

  /* --- FIONREAD --- */
  T_BEGIN("ioctl/fionread");
  {
    int sv[2];
    T_ASSERT_OK(mkpair(sv));
    T_ASSERT_EQ(write(sv[0], "12345", 5), 5);
    int avail = -1;
    T_ASSERT_OK(ioctl(sv[1], FIONREAD, &avail));
    T_ASSERT_EQ(avail, 5);
    char b[8];
    T_ASSERT_EQ(read(sv[1], b, 3), 3);
    avail = -1;
    T_ASSERT_OK(ioctl(sv[1], FIONREAD, &avail));
    T_ASSERT_EQ(avail, 2);
    close(sv[0]); close(sv[1]);
  }

  /* --- poll timeout precision --- */
  T_BEGIN("poll/timeout-precision");
  {
    int sv[2];
    T_ASSERT_OK(mkpair(sv));
    struct pollfd pfd = {sv[0], POLLIN, 0};
    long t0 = now_ms();
    int r2 = poll(&pfd, 1, 200);
    long dt = now_ms() - t0;
    T_ASSERT_EQ(r2, 0);
    T_ASSERT(dt >= 180 && dt <= 1000); /* not early, not wildly late */
    /* ready case returns promptly */
    T_ASSERT_EQ(write(sv[1], "z", 1), 1);
    t0 = now_ms();
    r2 = poll(&pfd, 1, 1000);
    dt = now_ms() - t0;
    T_ASSERT_EQ(r2, 1);
    T_ASSERT(pfd.revents & POLLIN);
    T_ASSERT(dt < 500);
    close(sv[0]); close(sv[1]);
  }

  /* --- poll HUP --- */
  T_BEGIN("poll/hup");
  {
    int sv[2];
    T_ASSERT_OK(mkpair(sv));
    close(sv[1]);
    struct pollfd pfd = {sv[0], POLLIN, 0};
    int r2 = poll(&pfd, 1, 500);
    T_ASSERT_EQ(r2, 1);
    T_ASSERT(pfd.revents & (POLLHUP | POLLIN));
    close(sv[0]);
  }

  return WTEST_END();
}
