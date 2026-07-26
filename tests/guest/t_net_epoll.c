/* t_net_epoll.c — epoll LT / ONESHOT / MOD / DEL / RDHUP semantics.
 *
 * Stream pairs use AF_UNIX socketpair when available; otherwise fall back
 * to a TCP loopback connection. */
#define _GNU_SOURCE
#include <sys/epoll.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <unistd.h>
#include "wtest.h"

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

static int have_afunix = -1;
static int mkpair(int sv[2]) {
  if (have_afunix != 0) {
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0) { have_afunix = 1; return 0; }
    have_afunix = 0;
  }
  return tcp_pair(sv);
}

int main(void) {
  /* explicit AF_UNIX probe: socketpair must exist (Linux parity) */
  T_BEGIN("epoll/afunix-socketpair");
  {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0) {
      have_afunix = 1;
      wtest_pass++; printf("PASS %s\n", wtest_cur);
      close(sv[0]); close(sv[1]);
    } else {
      have_afunix = 0;
      T_ASSERT_OK(socketpair(AF_UNIX, SOCK_STREAM, 0, sv)); /* FAIL w/ errno */
      T_SKIP("epoll pair transport", "AF_UNIX missing; using TCP loopback");
    }
  }
  int ep = epoll_create1(0);
  T_BEGIN("epoll/create");
  T_ASSERT(ep >= 0);
  if (ep < 0) return WTEST_END();
  struct epoll_event ev, out[4];

  /* --- LT (level-triggered): partial read re-triggers --- */
  T_BEGIN("epoll/lt-retrigger");
  {
    int sv[2];
    T_ASSERT_OK(mkpair(sv));
    ev.events = EPOLLIN; ev.data.u32 = 0xAA;
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_ADD, sv[0], &ev));
    T_ASSERT_EQ(write(sv[1], "abcd", 4), 4);
    int n = epoll_wait(ep, out, 4, 500);
    T_ASSERT_EQ(n, 1);
    T_ASSERT(out[0].data.u32 == 0xAA);
    char b[4];
    T_ASSERT_EQ(read(sv[0], b, 2), 2); /* leave 2 bytes */
    n = epoll_wait(ep, out, 4, 500);   /* LT must fire again */
    T_ASSERT_EQ(n, 1);
    T_ASSERT_EQ(read(sv[0], b, 2), 2);
    n = epoll_wait(ep, out, 4, 100);   /* drained: no more events */
    T_ASSERT_EQ(n, 0);
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_DEL, sv[0], NULL));
    close(sv[0]); close(sv[1]);
  }

  /* --- ONESHOT: fires once, rearms only via MOD --- */
  T_BEGIN("epoll/oneshot");
  {
    int sv[2];
    T_ASSERT_OK(mkpair(sv));
    ev.events = EPOLLIN | EPOLLONESHOT; ev.data.u32 = 1;
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_ADD, sv[0], &ev));
    T_ASSERT_EQ(write(sv[1], "zz", 2), 2);
    T_ASSERT_EQ(epoll_wait(ep, out, 4, 500), 1);
    /* still readable, but ONESHOT already fired: no second event */
    T_ASSERT_EQ(epoll_wait(ep, out, 4, 100), 0);
    /* rearm via MOD */
    ev.events = EPOLLIN | EPOLLONESHOT; ev.data.u32 = 2;
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_MOD, sv[0], &ev));
    int n = epoll_wait(ep, out, 4, 500);
    T_ASSERT_EQ(n, 1);
    T_ASSERT(out[0].data.u32 == 2);
    char b[2];
    T_ASSERT_EQ(read(sv[0], b, 2), 2);
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_DEL, sv[0], NULL));
    close(sv[0]); close(sv[1]);
  }

  /* --- MOD changes interest mask --- */
  T_BEGIN("epoll/mod-mask");
  {
    int sv[2];
    T_ASSERT_OK(mkpair(sv));
    ev.events = EPOLLOUT; ev.data.u32 = 3;
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_ADD, sv[0], &ev));
    /* writable: should fire for EPOLLOUT */
    T_ASSERT_EQ(epoll_wait(ep, out, 4, 500), 1);
    T_ASSERT(out[0].events & EPOLLOUT);
    /* switch interest to IN only: no data queued -> no events */
    ev.events = EPOLLIN; ev.data.u32 = 3;
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_MOD, sv[0], &ev));
    T_ASSERT_EQ(epoll_wait(ep, out, 4, 100), 0);
    T_ASSERT_EQ(write(sv[1], "k", 1), 1);
    T_ASSERT_EQ(epoll_wait(ep, out, 4, 500), 1);
    char b;
    T_ASSERT_EQ(read(sv[0], &b, 1), 1);
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_DEL, sv[0], NULL));
    close(sv[0]); close(sv[1]);
  }

  /* --- DEL stops reporting; ctl error cases --- */
  T_BEGIN("epoll/del-and-errors");
  {
    int sv[2];
    T_ASSERT_OK(mkpair(sv));
    ev.events = EPOLLIN; ev.data.u32 = 4;
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_ADD, sv[0], &ev));
    T_ASSERT_EQ(write(sv[1], "q", 1), 1);
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_DEL, sv[0], NULL));
    T_ASSERT_EQ(epoll_wait(ep, out, 4, 100), 0);
    char b;
    T_ASSERT_EQ(read(sv[0], &b, 1), 1);
    /* double ADD -> EEXIST; DEL of non-registered -> ENOENT */
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_ADD, sv[0], &ev));
    T_ASSERT_ERRNO(epoll_ctl(ep, EPOLL_CTL_ADD, sv[0], &ev), EEXIST);
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_DEL, sv[0], NULL));
    T_ASSERT_ERRNO(epoll_ctl(ep, EPOLL_CTL_DEL, sv[0], NULL), ENOENT);
    T_ASSERT_ERRNO(epoll_ctl(ep, EPOLL_CTL_MOD, sv[0], &ev), ENOENT);
    close(sv[0]); close(sv[1]);
  }

  /* --- RDHUP on peer shutdown --- */
  T_BEGIN("epoll/rdhup");
  {
    int sv[2];
    T_ASSERT_OK(mkpair(sv));
    ev.events = EPOLLIN | EPOLLRDHUP; ev.data.u32 = 5;
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_ADD, sv[0], &ev));
    T_ASSERT_OK(shutdown(sv[1], SHUT_WR));
    int n = epoll_wait(ep, out, 4, 500);
    T_ASSERT_EQ(n, 1);
    T_ASSERT(out[0].events & EPOLLRDHUP);
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_DEL, sv[0], NULL));
    close(sv[0]); close(sv[1]);
  }

  /* --- HUP delivered even without EPOLLIN interest --- */
  T_BEGIN("epoll/hup-implicit");
  {
    int sv[2];
    T_ASSERT_OK(mkpair(sv));
    ev.events = 0; ev.data.u32 = 6;
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_ADD, sv[0], &ev));
    close(sv[1]);
    int n = epoll_wait(ep, out, 4, 500);
    T_ASSERT_EQ(n, 1);
    T_ASSERT(out[0].events & (EPOLLHUP | EPOLLERR));
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_DEL, sv[0], NULL));
    close(sv[0]);
  }

  /* --- multiple fds in one wait --- */
  T_BEGIN("epoll/multi-fd");
  {
    int a[2], b[2];
    T_ASSERT_OK(mkpair(a));
    T_ASSERT_OK(mkpair(b));
    ev.events = EPOLLIN;
    ev.data.u32 = 10; T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_ADD, a[0], &ev));
    ev.data.u32 = 11; T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_ADD, b[0], &ev));
    T_ASSERT_EQ(write(a[1], "1", 1), 1);
    T_ASSERT_EQ(write(b[1], "2", 1), 1);
    int n = epoll_wait(ep, out, 4, 500);
    T_ASSERT_EQ(n, 2);
    int seen = 0;
    for (int i = 0; i < n; i++) seen |= 1 << (out[i].data.u32 - 10);
    T_ASSERT_EQ(seen, 3);
    close(a[0]); close(a[1]); close(b[0]); close(b[1]);
  }

  close(ep);
  return WTEST_END();
}
