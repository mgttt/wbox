/* t_signalfd.c - signalfd masks, readiness, sharing and consumption. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/signalfd.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/uio.h>
#include <sys/wait.h>
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

static int sfd_old(int fd, const sigset_t *mask, size_t size) {
  return syscall(SYS_signalfd, fd, mask, size);
}

static void Block(sigset_t *mask, int sig) {
  sigemptyset(mask);
  sigaddset(mask, sig);
  T_ASSERT_OK(sigprocmask(SIG_BLOCK, mask, NULL));
}

int main(void) {
  struct signalfd_siginfo si;
  sigset_t mask;

  T_BEGIN("signalfd/create-flags-and-errors");
  {
    int fd;
    sigset_t current;
    sigset_t impossible;
    sigemptyset(&impossible);
    sigaddset(&impossible, SIGKILL);
    sigaddset(&impossible, SIGSTOP);
    T_ASSERT_OK(syscall(SYS_rt_sigprocmask, SIG_BLOCK, &impossible, NULL,
                        KERNEL_SIGSET_SIZE));
    memset(&current, 0, sizeof current);
    T_ASSERT_OK(syscall(SYS_rt_sigprocmask, SIG_BLOCK, NULL, &current,
                        KERNEL_SIGSET_SIZE));
    T_ASSERT(!sigismember(&current, SIGKILL));
    T_ASSERT(!sigismember(&current, SIGSTOP));
    Block(&mask, SIGUSR1);
    fd = sfd4(-1, &mask, KERNEL_SIGSET_SIZE, SFD_NONBLOCK | SFD_CLOEXEC);
    T_ASSERT(fd >= 0);
    T_ASSERT(fcntl(fd, F_GETFL) & O_NONBLOCK);
    T_ASSERT(fcntl(fd, F_GETFD) & FD_CLOEXEC);
    T_ASSERT_ERRNO(read(fd, &si, sizeof si), EAGAIN);
    T_ASSERT_ERRNO(read(fd, &si, sizeof si - 1), EINVAL);
    T_ASSERT_ERRNO(write(fd, &si, sizeof si), EINVAL);
    T_ASSERT_ERRNO(sfd4(-1, &mask, KERNEL_SIGSET_SIZE - 1, 0), EINVAL);
    T_ASSERT_ERRNO(
        sfd4(-1, &mask, KERNEL_SIGSET_SIZE, 0x40000000), EINVAL);
    T_ASSERT_ERRNO(sfd4(9999, &mask, KERNEL_SIGSET_SIZE, 0), EBADF);
    T_ASSERT_ERRNO(sfd4(-1, NULL, KERNEL_SIGSET_SIZE, 0), EFAULT);
    close(fd);
  }

  T_BEGIN("signalfd/poll-read-and-pending");
  {
    int fd;
    struct pollfd p;
    Block(&mask, SIGUSR1);
    fd = sfd4(-1, &mask, KERNEL_SIGSET_SIZE, SFD_NONBLOCK);
    p = (struct pollfd){fd, POLLIN, 0};
    T_ASSERT(fd >= 0);
    T_ASSERT_OK(kill(getpid(), SIGUSR1));
    T_ASSERT_EQ(poll(&p, 1, 1000), 1);
    T_ASSERT(p.revents & POLLIN);
    memset(&si, 0, sizeof si);
    T_ASSERT_EQ(read(fd, &si, sizeof si), sizeof si);
    T_ASSERT_EQ(si.ssi_signo, SIGUSR1);
    T_ASSERT_EQ(si.ssi_code, SI_USER);
    T_ASSERT_EQ(si.ssi_pid, getpid());
    T_ASSERT_EQ(poll(&p, 1, 0), 0);
    T_ASSERT_ERRNO(read(fd, &si, sizeof si), EAGAIN);
    sigpending(&mask);
    T_ASSERT(!sigismember(&mask, SIGUSR1));
    close(fd);
  }

  T_BEGIN("signalfd/update-dup-readv-and-coalesce");
  {
    int fd;
    int fd2;
    uint8_t record[sizeof(struct signalfd_siginfo)] = {0};
    struct iovec iov[2] = {
        {record, 17},
        {record + 17, sizeof record - 17},
    };
    Block(&mask, SIGUSR1);
    fd = sfd_old(-1, &mask, KERNEL_SIGSET_SIZE);
    T_ASSERT(fd >= 0);
    Block(&mask, SIGUSR2);
    T_ASSERT_EQ(sfd4(fd, &mask, KERNEL_SIGSET_SIZE, 0), fd);
    fd2 = dup(fd);
    T_ASSERT(fd2 >= 0);
    T_ASSERT_OK(fcntl(fd, F_SETFL, O_NONBLOCK));
    T_ASSERT(fcntl(fd2, F_GETFL) & O_NONBLOCK);
    T_ASSERT_OK(kill(getpid(), SIGUSR2));
    T_ASSERT_OK(kill(getpid(), SIGUSR2));
    T_ASSERT_EQ(readv(fd2, iov, 2), sizeof record);
    memcpy(&si, record, sizeof si);
    T_ASSERT_EQ(si.ssi_signo, SIGUSR2);
    T_ASSERT_ERRNO(read(fd, &si, sizeof si), EAGAIN);
    close(fd2);
    close(fd);
  }

  T_BEGIN("signalfd/batched-read");
  {
    int fd;
    struct signalfd_siginfo infos[2];
    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);
    sigaddset(&mask, SIGUSR2);
    T_ASSERT_OK(sigprocmask(SIG_BLOCK, &mask, NULL));
    fd = sfd4(-1, &mask, KERNEL_SIGSET_SIZE, SFD_NONBLOCK);
    T_ASSERT(fd >= 0);
    T_ASSERT_OK(kill(getpid(), SIGUSR2));
    T_ASSERT_OK(kill(getpid(), SIGUSR1));
    memset(infos, 0, sizeof infos);
    T_ASSERT_EQ(read(fd, infos, sizeof infos), sizeof infos);
    T_ASSERT_EQ(infos[0].ssi_signo, SIGUSR1);
    T_ASSERT_EQ(infos[1].ssi_signo, SIGUSR2);
    close(fd);
  }

  T_BEGIN("signalfd/blocking-read-wakes-on-alarm");
  {
    int fd;
    struct itimerval timer;
    Block(&mask, SIGALRM);
    fd = sfd4(-1, &mask, KERNEL_SIGSET_SIZE, 0);
    T_ASSERT(fd >= 0);
    memset(&timer, 0, sizeof timer);
    timer.it_value.tv_usec = 50 * 1000;
    T_ASSERT_OK(setitimer(ITIMER_REAL, &timer, NULL));
    memset(&si, 0, sizeof si);
    T_ASSERT_EQ(read(fd, &si, sizeof si), sizeof si);
    T_ASSERT_EQ(si.ssi_signo, SIGALRM);
    T_ASSERT_EQ(si.ssi_code, SI_KERNEL);
    T_ASSERT_EQ(si.ssi_pid, 0);
    close(fd);
  }

  T_BEGIN("signalfd/overlapping-descriptions-consume-once");
  {
    int a;
    int b;
    Block(&mask, SIGUSR1);
    a = sfd4(-1, &mask, KERNEL_SIGSET_SIZE, SFD_NONBLOCK);
    b = sfd4(-1, &mask, KERNEL_SIGSET_SIZE, SFD_NONBLOCK);
    T_ASSERT(a >= 0);
    T_ASSERT(b >= 0);
    T_ASSERT_OK(kill(getpid(), SIGUSR1));
    T_ASSERT_EQ(read(a, &si, sizeof si), sizeof si);
    T_ASSERT_EQ(si.ssi_signo, SIGUSR1);
    T_ASSERT_ERRNO(read(b, &si, sizeof si), EAGAIN);
    close(b);
    close(a);
  }

  T_BEGIN("signalfd/unblock-delivers-and-clears-readiness");
  {
    int fd;
    struct sigaction sa;
    struct pollfd p;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = OnSignal;
    sigemptyset(&sa.sa_mask);
    T_ASSERT_OK(sigaction(SIGUSR1, &sa, NULL));
    Block(&mask, SIGUSR1);
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
  }

  T_BEGIN("signalfd/epoll-and-close-purge");
  {
    int ep;
    int fd;
    struct epoll_event ev = {.events = EPOLLIN, .data.u64 = 0x51fd};
    struct epoll_event out;
    Block(&mask, SIGUSR1);
    ep = epoll_create1(0);
    fd = sfd4(-1, &mask, KERNEL_SIGSET_SIZE, SFD_NONBLOCK);
    T_ASSERT(ep >= 0);
    T_ASSERT(fd >= 0);
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_ADD, fd, &ev));
    T_ASSERT_OK(kill(getpid(), SIGUSR1));
    T_ASSERT_EQ(epoll_wait(ep, &out, 1, 1000), 1);
    T_ASSERT_EQ(out.data.u64, 0x51fd);
    T_ASSERT_EQ(read(fd, &si, sizeof si), sizeof si);
    close(fd);
    fd = sfd4(-1, &mask, KERNEL_SIGSET_SIZE, SFD_NONBLOCK);
    T_ASSERT(fd >= 0);
    T_ASSERT_EQ(epoll_wait(ep, &out, 1, 0), 0);
    close(fd);
    close(ep);
  }

  T_BEGIN("signalfd/fork-inherited-description");
  {
    int fd;
    pid_t pid;
    int status = 0;
    Block(&mask, SIGUSR2);
    fd = sfd4(-1, &mask, KERNEL_SIGSET_SIZE, 0);
    T_ASSERT(fd >= 0);
    pid = fork();
    T_ASSERT(pid >= 0);
    if (pid == 0) {
      struct signalfd_siginfo child_si;
      if (kill(getpid(), SIGUSR2) == 0 &&
          read(fd, &child_si, sizeof child_si) == sizeof child_si &&
          child_si.ssi_signo == SIGUSR2) {
        _exit(0);
      }
      _exit(1);
    }
    if (pid > 0) {
      T_ASSERT_EQ(waitpid(pid, &status, 0), pid);
      T_ASSERT(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    }
    close(fd);
  }

  return WTEST_END();
}
