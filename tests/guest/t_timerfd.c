/* t_timerfd.c - timerfd create/set/get, sharing and readiness semantics. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/syscall.h>
#include <sys/timerfd.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <string.h>

#include "wtest.h"

static int tfd_create(int clockid, int flags) {
  return syscall(SYS_timerfd_create, clockid, flags);
}

static int tfd_settime(int fd, int flags, const struct itimerspec *new_value,
                       struct itimerspec *old_value) {
  return syscall(SYS_timerfd_settime, fd, flags, new_value, old_value);
}

static int tfd_gettime(int fd, struct itimerspec *value) {
  return syscall(SYS_timerfd_gettime, fd, value);
}

static int64_t Nanos(const struct timespec *ts) {
  return (int64_t)ts->tv_sec * 1000000000 + ts->tv_nsec;
}

int main(void) {
  struct itimerspec it;
  struct itimerspec old;
  uint64_t expirations;

  T_BEGIN("timerfd/create-flags-and-errors");
  {
    int fd = tfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK | TFD_CLOEXEC);
    int efd = eventfd(0, EFD_NONBLOCK);
    memset(&it, 0, sizeof it);
    T_ASSERT(fd >= 0);
    T_ASSERT(efd >= 0);
    T_ASSERT(fcntl(fd, F_GETFL) & O_NONBLOCK);
    T_ASSERT(fcntl(fd, F_GETFD) & FD_CLOEXEC);
    T_ASSERT_ERRNO(read(fd, &expirations, sizeof expirations), EAGAIN);
    T_ASSERT_ERRNO(read(fd, &expirations, sizeof expirations - 1), EINVAL);
    T_ASSERT_ERRNO(write(fd, &expirations, sizeof expirations), EINVAL);
    T_ASSERT_ERRNO(tfd_settime(fd, 0, NULL, NULL), EFAULT);
    T_ASSERT_ERRNO(tfd_gettime(fd, NULL), EFAULT);
    T_ASSERT_ERRNO(tfd_settime(-1, 0, &it, NULL), EBADF);
    T_ASSERT_ERRNO(tfd_gettime(-1, &old), EBADF);
    T_ASSERT_ERRNO(tfd_settime(efd, 0, &it, NULL), EINVAL);
    T_ASSERT_ERRNO(tfd_gettime(efd, &old), EINVAL);
    T_ASSERT_OK(tfd_settime(fd, TFD_TIMER_CANCEL_ON_SET, &it, NULL));
    T_ASSERT_ERRNO(tfd_create(-1, 0), EINVAL);
    T_ASSERT_ERRNO(tfd_create(CLOCK_MONOTONIC, 0x40000000), EINVAL);
    close(efd);
    close(fd);
  }

  T_BEGIN("timerfd/one-shot-poll-read");
  {
    int fd = tfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    struct pollfd p = {fd, POLLIN, 0};
    T_ASSERT(fd >= 0);
    it.it_interval.tv_sec = 0;
    it.it_interval.tv_nsec = 0;
    it.it_value.tv_sec = 0;
    it.it_value.tv_nsec = 100 * 1000 * 1000;
    T_ASSERT_OK(tfd_settime(fd, 0, &it, NULL));
    T_ASSERT_EQ(poll(&p, 1, 0), 0);
    T_ASSERT_EQ(poll(&p, 1, 1000), 1);
    T_ASSERT(p.revents & POLLIN);
    expirations = 0;
    T_ASSERT_EQ(read(fd, &expirations, sizeof expirations),
                sizeof expirations);
    T_ASSERT_EQ(expirations, 1);
    T_ASSERT_OK(tfd_gettime(fd, &old));
    T_ASSERT_EQ(Nanos(&old.it_value), 0);
    close(fd);
  }

  T_BEGIN("timerfd/periodic-and-disarm");
  {
    int fd = tfd_create(CLOCK_MONOTONIC, 0);
    struct timespec delay = {0, 170 * 1000 * 1000};
    T_ASSERT(fd >= 0);
    it.it_interval.tv_sec = 0;
    it.it_interval.tv_nsec = 40 * 1000 * 1000;
    it.it_value = it.it_interval;
    T_ASSERT_OK(tfd_settime(fd, 0, &it, NULL));
    T_ASSERT_OK(nanosleep(&delay, NULL));
    expirations = 0;
    T_ASSERT_EQ(read(fd, &expirations, sizeof expirations),
                sizeof expirations);
    T_ASSERT(expirations >= 4);
    T_ASSERT_OK(tfd_gettime(fd, &old));
    T_ASSERT_EQ(Nanos(&old.it_interval), 40 * 1000 * 1000);
    it.it_interval.tv_sec = 0;
    it.it_interval.tv_nsec = 0;
    it.it_value = it.it_interval;
    T_ASSERT_OK(tfd_settime(fd, 0, &it, &old));
    T_ASSERT(Nanos(&old.it_value) > 0);
    T_ASSERT_EQ(Nanos(&old.it_interval), 40 * 1000 * 1000);
    close(fd);
  }

  T_BEGIN("timerfd/absolute-and-invalid-timespec");
  {
    int fd = tfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    struct pollfd p = {fd, POLLIN, 0};
    T_ASSERT(fd >= 0);
    T_ASSERT_OK(clock_gettime(CLOCK_MONOTONIC, &it.it_value));
    it.it_interval.tv_sec = 0;
    it.it_interval.tv_nsec = 0;
    it.it_value.tv_nsec += 80 * 1000 * 1000;
    if (it.it_value.tv_nsec >= 1000000000) {
      ++it.it_value.tv_sec;
      it.it_value.tv_nsec -= 1000000000;
    }
    T_ASSERT_OK(tfd_settime(fd, TFD_TIMER_ABSTIME, &it, NULL));
    T_ASSERT_EQ(poll(&p, 1, 1000), 1);
    T_ASSERT_EQ(read(fd, &expirations, sizeof expirations),
                sizeof expirations);
    it.it_value.tv_sec = 0;
    it.it_value.tv_nsec = 1000000000;
    T_ASSERT_ERRNO(tfd_settime(fd, 0, &it, NULL), EINVAL);
    T_ASSERT_ERRNO(tfd_settime(fd, 0x40000000, &it, NULL), EINVAL);
    close(fd);
  }

  T_BEGIN("timerfd/dup-readv-and-shared-status");
  {
    int fd = tfd_create(CLOCK_MONOTONIC, 0);
    int fd2 = dup(fd);
    uint64_t value = 0;
    struct iovec iov[2] = {
        {(char *)&value, 3},
        {(char *)&value + 3, 5},
    };
    T_ASSERT(fd >= 0);
    T_ASSERT(fd2 >= 0);
    T_ASSERT_OK(fcntl(fd, F_SETFL, O_NONBLOCK));
    T_ASSERT(fcntl(fd2, F_GETFL) & O_NONBLOCK);
    it.it_interval.tv_sec = 0;
    it.it_interval.tv_nsec = 0;
    it.it_value.tv_sec = 0;
    it.it_value.tv_nsec = 50 * 1000 * 1000;
    T_ASSERT_OK(tfd_settime(fd2, 0, &it, NULL));
    T_ASSERT_EQ(lseek(fd, 10, SEEK_SET), 0);
    T_ASSERT_EQ(poll(&(struct pollfd){fd, POLLIN, 0}, 1, 1000), 1);
    T_ASSERT_EQ(readv(fd2, iov, 2), 8);
    T_ASSERT_EQ(value, 1);
    T_ASSERT_ERRNO(read(fd, &value, sizeof value), EAGAIN);
    close(fd2);
    close(fd);
  }

  T_BEGIN("timerfd/epoll-and-close-purge");
  {
    int ep = epoll_create1(0);
    int fd = tfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    struct epoll_event ev = {.events = EPOLLIN, .data.u64 = 0x71fd};
    struct epoll_event out;
    T_ASSERT(ep >= 0);
    T_ASSERT(fd >= 0);
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_ADD, fd, &ev));
    it.it_interval.tv_sec = 0;
    it.it_interval.tv_nsec = 0;
    it.it_value.tv_sec = 0;
    it.it_value.tv_nsec = 60 * 1000 * 1000;
    T_ASSERT_OK(tfd_settime(fd, 0, &it, NULL));
    T_ASSERT_EQ(epoll_wait(ep, &out, 1, 1000), 1);
    T_ASSERT_EQ(out.data.u64, 0x71fd);
    close(fd);
    fd = tfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    T_ASSERT(fd >= 0);
    T_ASSERT_EQ(epoll_wait(ep, &out, 1, 0), 0);
    close(fd);
    close(ep);
  }

  T_BEGIN("timerfd/fork-shares-description");
  {
    int fd = tfd_create(CLOCK_MONOTONIC, 0);
    pid_t pid;
    int status = 0;
    T_ASSERT(fd >= 0);
    it.it_interval.tv_sec = 0;
    it.it_interval.tv_nsec = 0;
    it.it_value.tv_sec = 0;
    it.it_value.tv_nsec = 80 * 1000 * 1000;
    T_ASSERT_OK(tfd_settime(fd, 0, &it, NULL));
    pid = fork();
    T_ASSERT(pid >= 0);
    if (pid == 0) {
      uint64_t child_value = 0;
      _exit(read(fd, &child_value, sizeof child_value) == 8 &&
                    child_value == 1
                ? 0
                : 1);
    }
    if (pid > 0) {
      T_ASSERT_EQ(waitpid(pid, &status, 0), pid);
      T_ASSERT(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    }
    close(fd);
  }

  T_BEGIN("timerfd/clock-variants");
  {
    int realtime = tfd_create(CLOCK_REALTIME, 0);
    int boottime = tfd_create(CLOCK_BOOTTIME, 0);
    T_ASSERT(realtime >= 0);
    T_ASSERT(boottime >= 0);
    close(realtime);
    close(boottime);
  }

  return WTEST_END();
}
