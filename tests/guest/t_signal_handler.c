/* t_signal_handler.c - delivery that must transfer control into guest code. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <string.h>
#include <sys/signalfd.h>
#include <sys/syscall.h>
#include <unistd.h>

#include "wtest.h"

#define KERNEL_SIGSET_SIZE 8

static volatile sig_atomic_t g_seen;

static void OnSignal(int sig) {
  if (sig == SIGUSR1) g_seen = 1;
}

static int sfd4(int fd, const sigset_t *mask, size_t size, int flags) {
  return syscall(SYS_signalfd4, fd, mask, size, flags);
}

int main(void) {
  struct signalfd_siginfo si;
  struct sigaction sa;
  struct pollfd p;
  sigset_t mask;
  int fd;

  T_BEGIN("signal-handler/unblock-delivers-and-clears-pending");
  memset(&sa, 0, sizeof sa);
  sa.sa_handler = OnSignal;
  sigemptyset(&sa.sa_mask);
  T_ASSERT_OK(sigaction(SIGUSR1, &sa, NULL));
  sigemptyset(&mask);
  sigaddset(&mask, SIGUSR1);
  T_ASSERT_OK(sigprocmask(SIG_BLOCK, &mask, NULL));
  fd = sfd4(-1, &mask, KERNEL_SIGSET_SIZE, SFD_NONBLOCK);
  p = (struct pollfd){fd, POLLIN, 0};
  T_ASSERT(fd >= 0);
  g_seen = 0;
  T_ASSERT_OK(kill(getpid(), SIGUSR1));
  T_ASSERT_EQ(poll(&p, 1, 1000), 1);
  T_ASSERT_OK(sigprocmask(SIG_UNBLOCK, &mask, NULL));
  T_ASSERT_EQ(g_seen, 1);
  T_ASSERT_EQ(poll(&p, 1, 0), 0);
  T_ASSERT_ERRNO(read(fd, &si, sizeof si), EAGAIN);
  T_ASSERT_OK(sigprocmask(SIG_BLOCK, &mask, NULL));
  close(fd);

  return WTEST_END();
}
