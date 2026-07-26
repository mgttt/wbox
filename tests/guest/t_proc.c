/* t_proc.c — fork matrix, waitpid semantics, exit codes, signals. */
#define _GNU_SOURCE
#include <sys/resource.h>
#include <sys/wait.h>
#include <signal.h>
#include <unistd.h>
#include <stdlib.h>
#include "wtest.h"

static volatile sig_atomic_t got_chld = 0;
static void on_chld(int sig) { (void)sig; got_chld = 1; }

static volatile sig_atomic_t got_usr1 = 0;
static void on_usr1(int sig) { (void)sig; got_usr1 = 1; }

int main(void) {
  /* --- basic fork + exit code --- */
  T_BEGIN("proc/exit-code");
  pid_t pid = fork();
  T_ASSERT(pid >= 0);
  if (pid == 0) _exit(42);
  int st;
  T_ASSERT_EQ(waitpid(pid, &st, 0), pid);
  T_ASSERT(WIFEXITED(st) && WEXITSTATUS(st) == 42);
  T_ASSERT(!WIFSIGNALED(st));

  /* --- waitpid WNOHANG --- */
  T_BEGIN("proc/waitpid-wnohang");
  pid = fork();
  T_ASSERT(pid >= 0);
  if (pid == 0) { usleep(300 * 1000); _exit(7); }
  T_ASSERT_EQ(waitpid(pid, &st, WNOHANG), 0); /* still running */
  T_ASSERT_EQ(waitpid(pid, &st, 0), pid);
  T_ASSERT(WIFEXITED(st) && WEXITSTATUS(st) == 7);
  /* reaping again must give ECHILD */
  T_ASSERT_ERRNO(waitpid(pid, &st, 0), ECHILD);

  /* --- two concurrent children, wait in reverse order --- */
  T_BEGIN("proc/two-children-order");
  pid_t c1 = fork();
  T_ASSERT(c1 >= 0);
  if (c1 == 0) { usleep(200 * 1000); _exit(1); }
  pid_t c2 = fork();
  T_ASSERT(c2 >= 0);
  if (c2 == 0) { usleep(50 * 1000); _exit(2); }
  T_ASSERT_EQ(waitpid(c2, &st, 0), c2);
  T_ASSERT(WIFEXITED(st) && WEXITSTATUS(st) == 2);
  T_ASSERT_EQ(waitpid(c1, &st, 0), c1);
  T_ASSERT(WIFEXITED(st) && WEXITSTATUS(st) == 1);

  /* --- wait() collects any child --- */
  T_BEGIN("proc/wait-any");
  c1 = fork();
  if (c1 == 0) _exit(11);
  c2 = fork();
  if (c2 == 0) _exit(12);
  int seen = 0;
  for (int i = 0; i < 2; i++) {
    T_ASSERT(wait(&st) > 0);
    if (WIFEXITED(st) && (WEXITSTATUS(st) == 11 || WEXITSTATUS(st) == 12))
      seen++;
  }
  T_ASSERT_EQ(seen, 2);

  /* --- nested fork depth 5 --- */
  T_BEGIN("proc/nested-depth5");
  {
    int depth = 0;
    pid_t p = 1;
    for (int i = 0; i < 5 && p > 0; i++) {
      p = fork();
      if (p == 0) depth++;
    }
    if (p == 0) _exit(depth);
    /* parent: collect the chain */
    int ok = 1;
    for (int i = 0; i < 5; i++) {
      pid_t w = wait(&st);
      if (w <= 0 || !WIFEXITED(st)) { ok = 0; break; }
    }
    T_ASSERT(ok);
  }

  /* --- kill + wait status WIFSIGNALED --- */
  T_BEGIN("proc/kill-term-status");
  pid = fork();
  T_ASSERT(pid >= 0);
  if (pid == 0) { for (;;) pause(); }
  usleep(100 * 1000);
  T_ASSERT_OK(kill(pid, SIGTERM));
  T_ASSERT_EQ(waitpid(pid, &st, 0), pid);
  T_ASSERT(WIFSIGNALED(st) && WTERMSIG(st) == SIGTERM);

  T_BEGIN("proc/kill-sigkill");
  pid = fork();
  if (pid == 0) { for (;;) pause(); }
  usleep(100 * 1000);
  T_ASSERT_OK(kill(pid, SIGKILL));
  T_ASSERT_EQ(waitpid(pid, &st, 0), pid);
  T_ASSERT(WIFSIGNALED(st) && WTERMSIG(st) == SIGKILL);

  T_BEGIN("proc/kill-nonexistent-ESRCH");
  T_ASSERT_ERRNO(kill(999999, SIGTERM), ESRCH);

  /* --- signal delivery to child: SIGUSR1 handler --- */
  T_BEGIN("proc/signal-delivery-usr1");
  {
    int pp[2];
    T_ASSERT_OK(pipe(pp));
    pid = fork();
    if (pid == 0) {
      close(pp[0]);
      signal(SIGUSR1, on_usr1);
      char r = 'R';
      (void)!write(pp[1], &r, 1); /* tell parent we're ready */
      for (int i = 0; i < 100 && !got_usr1; i++) usleep(10 * 1000);
      char v = got_usr1 ? 'Y' : 'N';
      (void)!write(pp[1], &v, 1);
      _exit(0);
    }
    close(pp[1]);
    char r, v;
    T_ASSERT_EQ(read(pp[0], &r, 1), 1);
    usleep(50 * 1000);
    T_ASSERT_OK(kill(pid, SIGUSR1));
    T_ASSERT_EQ(read(pp[0], &v, 1), 1);
    T_ASSERT(v == 'Y');
    waitpid(pid, &st, 0);
    close(pp[0]);
  }

  /* --- SIGCHLD timing: handler fires before/with child exit --- */
  T_BEGIN("proc/sigchld-timing");
  {
    got_chld = 0;
    struct sigaction sa = {0};
    sa.sa_handler = on_chld;
    sigemptyset(&sa.sa_mask);
    T_ASSERT_OK(sigaction(SIGCHLD, &sa, NULL));
    pid = fork();
    if (pid == 0) _exit(0);
    int fired = 0;
    for (int i = 0; i < 200; i++) {
      if (got_chld) { fired = 1; break; }
      usleep(10 * 1000);
    }
    T_ASSERT(fired);
    T_ASSERT_EQ(waitpid(pid, &st, 0), pid);
    signal(SIGCHLD, SIG_DFL);
  }

  /* --- orphan reaping: child exits leaving grandchild; grandchild must be
   * reparented (to init/subreaper) and eventually reaped, not leaked as
   * eternal zombie / never-killable --- */
  T_BEGIN("proc/orphan-reparent");
  {
    int pp[2];
    T_ASSERT_OK(pipe(pp));
    pid = fork();
    if (pid == 0) {
      pid_t g = fork();
      if (g == 0) {
        usleep(300 * 1000); /* outlive parent */
        pid_t np = getppid();
        char b = (np == 1) ? '1' : (np > 0 ? 'R' : '?');
        (void)!write(pp[1], &b, 1);
        _exit(0);
      }
      _exit(0); /* parent dies immediately -> grandchild orphaned */
    }
    close(pp[1]);
    char b = '?';
    T_ASSERT_EQ(read(pp[0], &b, 1), 1);
    T_ASSERT(b == '1' || b == 'R'); /* reparented to init or subreaper */
    close(pp[0]);
    waitpid(pid, &st, 0);
  }

  /* --- fork pipe EOF semantics: reader sees EOF when writer child exits --- */
  T_BEGIN("proc/pipe-eof-after-fork");
  {
    int pp[2];
    T_ASSERT_OK(pipe(pp));
    pid = fork();
    if (pid == 0) {
      close(pp[0]);
      (void)!write(pp[1], "q", 1);
      _exit(0);
    }
    close(pp[1]);
    char b;
    T_ASSERT_EQ(read(pp[0], &b, 1), 1);
    T_ASSERT_EQ(read(pp[0], &b, 1), 0); /* EOF */
    waitpid(pid, &st, 0);
    close(pp[0]);
  }

  T_BEGIN("proc/prlimit-live-child");
  {
    int ctl[2], obs[2];
    struct rlimit own, old, replaced, lim, seen;
    T_ASSERT_OK(pipe(ctl));
    T_ASSERT_OK(pipe(obs));
    pid = fork();
    T_ASSERT(pid >= 0);
    if (pid == 0) {
      char byte = 'R';
      close(ctl[1]);
      close(obs[0]);
      if (write(obs[1], &byte, 1) != 1) _exit(1);
      if (read(ctl[0], &byte, 1) != 1) _exit(2);
      if (getrlimit(RLIMIT_NOFILE, &seen) == -1) _exit(3);
      if (write(obs[1], &seen, sizeof(seen)) != sizeof(seen)) _exit(4);
      _exit(0);
    }
    close(ctl[0]);
    close(obs[1]);
    char byte;
    T_ASSERT_EQ(read(obs[0], &byte, 1), 1);
    T_ASSERT_EQ(byte, 'R');
    T_ASSERT_OK(getrlimit(RLIMIT_NOFILE, &own));
    T_ASSERT_OK(prlimit(pid, RLIMIT_NOFILE, NULL, &old));
    T_ASSERT(old.rlim_cur == own.rlim_cur && old.rlim_max == own.rlim_max);
    T_ASSERT(old.rlim_cur > 0);
    lim = old;
    --lim.rlim_cur;
    T_ASSERT_OK(prlimit(pid, RLIMIT_NOFILE, &lim, &replaced));
    T_ASSERT(replaced.rlim_cur == old.rlim_cur &&
             replaced.rlim_max == old.rlim_max);
    T_ASSERT_EQ(write(ctl[1], "G", 1), 1);
    T_ASSERT_EQ(read(obs[0], &seen, sizeof(seen)), sizeof(seen));
    T_ASSERT(seen.rlim_cur == lim.rlim_cur && seen.rlim_max == lim.rlim_max);
    T_ASSERT_EQ(waitpid(pid, &st, 0), pid);
    T_ASSERT(WIFEXITED(st) && WEXITSTATUS(st) == 0);
    T_ASSERT_ERRNO(prlimit(pid, RLIMIT_NOFILE, NULL, &old), ESRCH);
    close(ctl[1]);
    close(obs[0]);
  }

  return WTEST_END();
}
