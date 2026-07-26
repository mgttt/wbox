/* t_signal_timer.c - alarm/setitimer delivery and interruptible waits. */
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "wtest.h"

static volatile sig_atomic_t g_alarms;

static void OnAlarm(int sig) {
  (void)sig;
  ++g_alarms;
}

static int InstallHandler(void) {
  struct sigaction sa;
  sa.sa_handler = OnAlarm;
  sigemptyset(&sa.sa_mask);
  sa.sa_flags = 0;
  return sigaction(SIGALRM, &sa, NULL);
}

static int64_t Millis(const struct timeval *tv) {
  return (int64_t)tv->tv_sec * 1000 + tv->tv_usec / 1000;
}

int main(void) {
  struct itimerval it;
  struct itimerval old;
  struct timespec req;
  struct timespec rem;

  T_ASSERT_OK(InstallHandler());

  T_BEGIN("signal-timer/alarm-pause");
  {
    g_alarms = 0;
    T_ASSERT_EQ(alarm(1), 0);
    T_ASSERT_ERRNO(pause(), EINTR);
    T_ASSERT_EQ(g_alarms, 1);
    T_ASSERT_EQ(alarm(0), 0);
  }

  T_BEGIN("signal-timer/alarm-cancel-reports-remaining");
  {
    unsigned remaining;
    g_alarms = 0;
    T_ASSERT_EQ(alarm(2), 0);
    remaining = alarm(0);
    T_ASSERT(remaining >= 1 && remaining <= 2);
    req.tv_sec = 0;
    req.tv_nsec = 150 * 1000 * 1000;
    T_ASSERT_OK(nanosleep(&req, NULL));
    T_ASSERT_EQ(g_alarms, 0);
  }

  T_BEGIN("signal-timer/setitimer-one-shot-and-get");
  {
    g_alarms = 0;
    it.it_interval.tv_sec = 0;
    it.it_interval.tv_usec = 0;
    it.it_value.tv_sec = 0;
    it.it_value.tv_usec = 150000;
    T_ASSERT_OK(setitimer(ITIMER_REAL, &it, NULL));
    T_ASSERT_OK(getitimer(ITIMER_REAL, &old));
    T_ASSERT(Millis(&old.it_value) > 0);
    T_ASSERT(Millis(&old.it_value) <= 150);
    T_ASSERT_ERRNO(pause(), EINTR);
    T_ASSERT_EQ(g_alarms, 1);
    T_ASSERT_OK(getitimer(ITIMER_REAL, &old));
    T_ASSERT_EQ(Millis(&old.it_value), 0);
  }

  T_BEGIN("signal-timer/periodic");
  {
    g_alarms = 0;
    it.it_interval.tv_sec = 0;
    it.it_interval.tv_usec = 75000;
    it.it_value = it.it_interval;
    T_ASSERT_OK(setitimer(ITIMER_REAL, &it, NULL));
    T_ASSERT_ERRNO(pause(), EINTR);
    T_ASSERT_ERRNO(pause(), EINTR);
    T_ASSERT(g_alarms >= 2);
    it.it_interval.tv_sec = 0;
    it.it_interval.tv_usec = 0;
    it.it_value = it.it_interval;
    T_ASSERT_OK(setitimer(ITIMER_REAL, &it, &old));
    T_ASSERT_EQ(Millis(&old.it_interval), 75);
  }

  T_BEGIN("signal-timer/nanosleep-interrupted");
  {
    g_alarms = 0;
    it.it_interval.tv_sec = 0;
    it.it_interval.tv_usec = 0;
    it.it_value.tv_sec = 0;
    it.it_value.tv_usec = 100000;
    T_ASSERT_OK(setitimer(ITIMER_REAL, &it, NULL));
    req.tv_sec = 2;
    req.tv_nsec = 0;
    rem.tv_sec = 0;
    rem.tv_nsec = 0;
    T_ASSERT_ERRNO(nanosleep(&req, &rem), EINTR);
    T_ASSERT_EQ(g_alarms, 1);
    T_ASSERT(rem.tv_sec >= 1);
  }

  T_BEGIN("signal-timer/fork-does-not-inherit");
  {
    pid_t pid;
    int status = 0;
    it.it_interval.tv_sec = 0;
    it.it_interval.tv_usec = 0;
    it.it_value.tv_sec = 3;
    it.it_value.tv_usec = 0;
    T_ASSERT_OK(setitimer(ITIMER_REAL, &it, NULL));
    pid = fork();
    T_ASSERT(pid >= 0);
    if (pid == 0) {
      struct itimerval child;
      int ok = getitimer(ITIMER_REAL, &child) == 0 &&
               Millis(&child.it_value) == 0 &&
               Millis(&child.it_interval) == 0;
      _exit(ok ? 0 : 1);
    }
    if (pid > 0) {
      unsigned remaining;
      T_ASSERT_EQ(waitpid(pid, &status, 0), pid);
      T_ASSERT(WIFEXITED(status) && WEXITSTATUS(status) == 0);
      T_ASSERT_OK(getitimer(ITIMER_REAL, &old));
      T_ASSERT(Millis(&old.it_value) > 0);
      remaining = alarm(0);
      T_ASSERT(remaining > 0);
      T_ASSERT(remaining <=
               (unsigned)(old.it_value.tv_sec + !!old.it_value.tv_usec));
    }
  }

  T_BEGIN("signal-timer/invalid-timeval");
  {
    it.it_interval.tv_sec = 0;
    it.it_interval.tv_usec = 0;
    it.it_value.tv_sec = 0;
    it.it_value.tv_usec = 1000000;
    T_ASSERT_ERRNO(setitimer(ITIMER_REAL, &it, NULL), EINVAL);
  }

  return WTEST_END();
}
