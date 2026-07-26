/* t_eventfd.c - Linux eventfd/eventfd2 counter, flags, dup and readiness. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <sys/epoll.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#include "wtest.h"

#ifndef EFD_SEMAPHORE
#define EFD_SEMAPHORE 1
#define EFD_CLOEXEC O_CLOEXEC
#define EFD_NONBLOCK O_NONBLOCK
#endif

static int efd(unsigned int init, int flags) {
  return syscall(SYS_eventfd2, init, flags);
}

int main(void) {
  uint64_t value;

  T_BEGIN("eventfd/create-read-initial");
  {
    int fd = syscall(SYS_eventfd, 7);
    T_ASSERT(fd >= 0);
    value = 0;
    T_ASSERT_EQ(read(fd, &value, sizeof value), sizeof value);
    T_ASSERT_EQ(value, 7);
    close(fd);
  }

  T_BEGIN("eventfd/nonblock-and-size");
  {
    int fd = efd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    T_ASSERT(fd >= 0);
    T_ASSERT(fcntl(fd, F_GETFL) & O_NONBLOCK);
    T_ASSERT(fcntl(fd, F_GETFD) & FD_CLOEXEC);
    T_ASSERT_ERRNO(read(fd, &value, sizeof value), EAGAIN);
    T_ASSERT_ERRNO(read(fd, &value, sizeof value - 1), EINVAL);
    T_ASSERT_ERRNO(write(fd, &value, sizeof value - 1), EINVAL);
    T_ASSERT_ERRNO(efd(0, 0x40000000), EINVAL);
    close(fd);
  }

  T_BEGIN("eventfd/accumulate-and-dup");
  {
    int fd = efd(0, EFD_NONBLOCK);
    int fd2 = dup(fd);
    uint64_t a = 2, b = 3;
    T_ASSERT(fd >= 0);
    T_ASSERT(fd2 >= 0);
    T_ASSERT_EQ(write(fd, &a, sizeof a), sizeof a);
    T_ASSERT_EQ(write(fd2, &b, sizeof b), sizeof b);
    value = 0;
    T_ASSERT_EQ(read(fd2, &value, sizeof value), sizeof value);
    T_ASSERT_EQ(value, 5);
    T_ASSERT_ERRNO(read(fd, &value, sizeof value), EAGAIN);
    close(fd2);
    close(fd);
  }

  T_BEGIN("eventfd/readv-writev-and-shared-status");
  {
    int fd = efd(0, 0);
    int fd2 = dup(fd);
    uint64_t input = 0x0102030405060708ull;
    uint64_t output = 0;
    struct iovec wiov[2] = {
        {(char *)&input, 3},
        {(char *)&input + 3, 5},
    };
    struct iovec riov[2] = {
        {(char *)&output, 4},
        {(char *)&output + 4, 4},
    };
    T_ASSERT(fd >= 0);
    T_ASSERT(fd2 >= 0);
    T_ASSERT_EQ(writev(fd, wiov, 2), 8);
    T_ASSERT_EQ(readv(fd2, riov, 2), 8);
    T_ASSERT_EQ(output, input);
    T_ASSERT_OK(fcntl(fd, F_SETFL, O_NONBLOCK));
    T_ASSERT(fcntl(fd2, F_GETFL) & O_NONBLOCK);
    T_ASSERT_ERRNO(read(fd2, &output, sizeof output), EAGAIN);
    close(fd2);
    close(fd);
  }

  T_BEGIN("eventfd/semaphore");
  {
    int fd = efd(2, EFD_NONBLOCK | EFD_SEMAPHORE);
    T_ASSERT(fd >= 0);
    value = 0;
    T_ASSERT_EQ(read(fd, &value, sizeof value), sizeof value);
    T_ASSERT_EQ(value, 1);
    value = 0;
    T_ASSERT_EQ(read(fd, &value, sizeof value), sizeof value);
    T_ASSERT_EQ(value, 1);
    T_ASSERT_ERRNO(read(fd, &value, sizeof value), EAGAIN);
    close(fd);
  }

  T_BEGIN("eventfd/noop-seek-and-positioned-io");
  {
    int fd = efd(4, 0);
    uint64_t two = 2;
    T_ASSERT(fd >= 0);
    T_ASSERT_EQ(lseek(fd, 123, SEEK_SET), 0);
    value = 0;
    T_ASSERT_EQ(pread(fd, &value, sizeof value, 99), sizeof value);
    T_ASSERT_EQ(value, 4);
    T_ASSERT_EQ(pwrite(fd, &two, sizeof two, 77), sizeof two);
    value = 0;
    T_ASSERT_EQ(read(fd, &value, sizeof value), sizeof value);
    T_ASSERT_EQ(value, 2);
    close(fd);
  }

  T_BEGIN("eventfd/poll-and-epoll");
  {
    int fd = efd(0, EFD_NONBLOCK);
    int ep = epoll_create1(0);
    struct pollfd p = {fd, POLLIN | POLLOUT, 0};
    struct epoll_event ev = {.events = EPOLLIN, .data.u64 = 0xefd};
    struct epoll_event out;
    T_ASSERT(fd >= 0);
    T_ASSERT(ep >= 0);
    T_ASSERT_EQ(poll(&p, 1, 0), 1);
    T_ASSERT(!(p.revents & POLLIN));
    T_ASSERT(p.revents & POLLOUT);
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_ADD, fd, &ev));
    T_ASSERT_EQ(epoll_wait(ep, &out, 1, 0), 0);
    value = 9;
    T_ASSERT_EQ(write(fd, &value, sizeof value), sizeof value);
    T_ASSERT_EQ(epoll_wait(ep, &out, 1, 500), 1);
    T_ASSERT_EQ(out.data.u64, 0xefd);
    T_ASSERT(out.events & EPOLLIN);
    T_ASSERT_EQ(read(fd, &value, sizeof value), sizeof value);
    T_ASSERT_EQ(epoll_wait(ep, &out, 1, 0), 0);
    close(ep);
    close(fd);
  }

  T_BEGIN("eventfd/epollet-rearms-after-drain");
  {
    int fd = efd(0, EFD_NONBLOCK);
    int ep = epoll_create1(0);
    struct epoll_event ev = {
        .events = EPOLLIN | EPOLLET,
        .data.u64 = 0xe7fd,
    };
    struct epoll_event out;
    T_ASSERT(fd >= 0);
    T_ASSERT(ep >= 0);
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_ADD, fd, &ev));
    value = 1;
    T_ASSERT_EQ(write(fd, &value, sizeof value), sizeof value);
    T_ASSERT_EQ(epoll_wait(ep, &out, 1, 500), 1);
    T_ASSERT_EQ(epoll_wait(ep, &out, 1, 100), 0);
    T_ASSERT_EQ(read(fd, &value, sizeof value), sizeof value);
    value = 2;
    T_ASSERT_EQ(write(fd, &value, sizeof value), sizeof value);
    T_ASSERT_EQ(epoll_wait(ep, &out, 1, 500), 1);
    T_ASSERT_EQ(out.data.u64, 0xe7fd);
    T_ASSERT_EQ(read(fd, &value, sizeof value), sizeof value);
    T_ASSERT_EQ(value, 2);
    close(ep);
    close(fd);
  }

  T_BEGIN("eventfd/epoll-close-purge");
  {
    int ep = epoll_create1(0);
    int fd = efd(0, EFD_NONBLOCK);
    struct epoll_event ev = {.events = EPOLLIN, .data.u64 = 1};
    struct epoll_event out;
    T_ASSERT(ep >= 0);
    T_ASSERT(fd >= 0);
    T_ASSERT_OK(epoll_ctl(ep, EPOLL_CTL_ADD, fd, &ev));
    close(fd);
    fd = efd(1, EFD_NONBLOCK);
    T_ASSERT(fd >= 0);
    T_ASSERT_EQ(epoll_wait(ep, &out, 1, 0), 0);
    close(fd);
    close(ep);
  }

  T_BEGIN("eventfd/fork-shared-counter");
  {
    int fd = efd(0, 0);
    pid_t pid = fork();
    T_ASSERT(fd >= 0);
    T_ASSERT(pid >= 0);
    if (pid == 0) {
      uint64_t child_value = 13;
      _exit(write(fd, &child_value, sizeof child_value) == 8 ? 0 : 1);
    }
    if (pid > 0) {
      int status = 0;
      value = 0;
      T_ASSERT_EQ(read(fd, &value, sizeof value), sizeof value);
      T_ASSERT_EQ(value, 13);
      T_ASSERT_EQ(waitpid(pid, &status, 0), pid);
      T_ASSERT(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    }
    close(fd);
  }

  T_BEGIN("eventfd/overflow-boundary");
  {
    int fd = efd(0, EFD_NONBLOCK);
    uint64_t max = UINT64_MAX - 1;
    uint64_t one = 1;
    uint64_t invalid = UINT64_MAX;
    T_ASSERT(fd >= 0);
    T_ASSERT_EQ(write(fd, &max, sizeof max), sizeof max);
    T_ASSERT_ERRNO(write(fd, &one, sizeof one), EAGAIN);
    T_ASSERT_ERRNO(write(fd, &invalid, sizeof invalid), EINVAL);
    value = 0;
    T_ASSERT_EQ(read(fd, &value, sizeof value), sizeof value);
    T_ASSERT_EQ(value, max);
    close(fd);
  }

  return WTEST_END();
}
