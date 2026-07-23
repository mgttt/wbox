// wbox win32: signal layer.
// L1: sigaction/sigprocmask are record-only stubs (guest signal semantics
// are emulated inside blink; host async signals don't exist on win32).
// A VEH converts host synchronous exceptions to a diagnostic abort, and
// Ctrl+C/Close console events terminate the process.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <windows.h>

#include "win32.h"

static struct sigaction g_sa[NSIG];
static sigset_t g_mask;

int sigaction(int sig, const struct sigaction *n, struct sigaction *o) {
  if (sig <= 0 || sig >= NSIG) {
    errno = EINVAL;
    return -1;
  }
  if (o) *o = g_sa[sig];
  if (n) g_sa[sig] = *n;
  return 0;
}

int sigprocmask(int how, const sigset_t *n, sigset_t *o) {
  if (o) *o = g_mask;
  if (n) {
    switch (how) {
      case SIG_BLOCK:
        for (int i = 0; i < 2; ++i) g_mask.__bits[i] |= n->__bits[i];
        break;
      case SIG_UNBLOCK:
        for (int i = 0; i < 2; ++i) g_mask.__bits[i] &= ~n->__bits[i];
        break;
      case SIG_SETMASK:
        g_mask = *n;
        break;
      default:
        errno = EINVAL;
        return -1;
    }
  }
  return 0;
}

int sigemptyset(sigset_t *s) {
  memset(s, 0, sizeof(*s));
  return 0;
}

int sigfillset(sigset_t *s) {
  memset(s, 0xff, sizeof(*s));
  return 0;
}

int sigaddset(sigset_t *s, int sig) {
  if (sig <= 0 || sig >= NSIG) {
    errno = EINVAL;
    return -1;
  }
  s->__bits[(sig - 1) / 64] |= 1UL << ((sig - 1) % 64);
  return 0;
}

int sigdelset(sigset_t *s, int sig) {
  if (sig <= 0 || sig >= NSIG) {
    errno = EINVAL;
    return -1;
  }
  s->__bits[(sig - 1) / 64] &= ~(1UL << ((sig - 1) % 64));
  return 0;
}

int sigismember(const sigset_t *s, int sig) {
  if (sig <= 0 || sig >= NSIG) return 0;
  return !!(s->__bits[(sig - 1) / 64] & (1UL << ((sig - 1) % 64)));
}

int sigsuspend(const sigset_t *mask) {
  Sleep(INFINITE);
  errno = EINTR;
  return -1;
}

int sigpending(sigset_t *s) {
  sigemptyset(s);
  return 0;
}

int sigaltstack(const stack_t *n, stack_t *o) {
  if (o) {
    o->ss_sp = NULL;
    o->ss_flags = SS_DISABLE;
    o->ss_size = 0;
  }
  return 0;
}

int kill(pid_t pid, int sig) {
  if (pid == getpid() || pid == 0 || pid == -1) {
    // L1: fatal-by-default semantics
    if (sig == SIGKILL || sig == SIGTERM || sig == SIGINT) {
      ExitProcess(128 + sig);
    }
    return 0;
  }
  errno = ESRCH;
  return -1;
}

int killpg(pid_t pgrp, int sig) {
  return kill(-1, sig);
}

int sigqueue(pid_t pid, int sig, const union sigval v) {
  return kill(pid, sig);
}

int sighold(int sig) { return 0; }
int sigrelse(int sig) { return 0; }
int sigignore(int sig) {
  struct sigaction sa;
  memset(&sa, 0, sizeof(sa));
  sa.sa_handler = SIG_IGN;
  return sigaction(sig, &sa, NULL);
}

int pthread_sigmask(int how, const sigset_t *n, sigset_t *o) {
  return sigprocmask(how, n, o);
}

int wbox_pthread_kill(uintptr_t thr, int sig) {
  return 0;
}

char *strsignal(int sig) {
  static char buf[32];
  switch (sig) {
    case SIGINT: return (char *)"Interrupt";
    case SIGILL: return (char *)"Illegal instruction";
    case SIGABRT: return (char *)"Aborted";
    case SIGFPE: return (char *)"Floating point exception";
    case SIGSEGV: return (char *)"Segmentation fault";
    case SIGTERM: return (char *)"Terminated";
    case SIGKILL: return (char *)"Killed";
    case SIGPIPE: return (char *)"Broken pipe";
    case SIGCHLD: return (char *)"Child exited";
    case SIGWINCH: return (char *)"Window size changed";
    default:
      snprintf(buf, sizeof(buf), "Signal %d", sig);
      return buf;
  }
}

// ---------------------------------------------------------------- VEH

static LONG WINAPI WboxVeh(EXCEPTION_POINTERS *ep) {
  DWORD code = ep->ExceptionRecord->ExceptionCode;
  void *addr = ep->ExceptionRecord->ExceptionAddress;
  switch (code) {
    case EXCEPTION_ACCESS_VIOLATION:
    case EXCEPTION_ILLEGAL_INSTRUCTION:
    case EXCEPTION_INT_DIVIDE_BY_ZERO:
    case EXCEPTION_FLT_DIVIDE_BY_ZERO:
    case EXCEPTION_STACK_OVERFLOW:
      fprintf(stderr,
              "wbox-linux: fatal host exception %#lx at %p (rip=%p)\n", code,
              addr, (void *)ep->ContextRecord->Rip);
      ExitProcess(128 + SIGSEGV);
      return EXCEPTION_CONTINUE_SEARCH;
    default:
      return EXCEPTION_CONTINUE_SEARCH;
  }
}

static BOOL WINAPI WboxCtrlHandler(DWORD type) {
  switch (type) {
    case CTRL_C_EVENT:
    case CTRL_BREAK_EVENT:
      ExitProcess(128 + SIGINT);
      return TRUE;
    case CTRL_CLOSE_EVENT:
      ExitProcess(128 + SIGHUP);
      return TRUE;
    default:
      return FALSE;
  }
}

void WboxSigInit(void) {
  AddVectoredExceptionHandler(1, WboxVeh);
  SetConsoleCtrlHandler(WboxCtrlHandler, TRUE);
}
