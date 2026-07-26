// wbox win32: signal layer.
// L1: sigaction/sigprocmask are record-only stubs (guest signal semantics
// are emulated inside blink; host async signals don't exist on win32).
// A VEH converts host synchronous exceptions to a diagnostic abort, and
// Ctrl+C/Close console events terminate the process.
#include <errno.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <windows.h>

#include "blink/machine.h"
#include "blink/tunables.h"
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

// Crash diagnostics. Runs ONLY on a fatal host exception (zero cost on
// the normal path): reports the host fault, the guest PC of the crashing
// thread, the guest fault address (translated through the thread's VA
// window) and the guest executable it maps into. WBOX_DEBUG=1 adds the
// host register dump and window layout.
static void WboxVehDiagnose(EXCEPTION_POINTERS *ep) {
  DWORD code = ep->ExceptionRecord->ExceptionCode;
  CONTEXT *ctx = ep->ContextRecord;
  struct Machine *m;
  uintptr_t fa = 0;
  int have_fa = 0;
  const char *what = "exception";
  switch (code) {
    case EXCEPTION_ACCESS_VIOLATION: {
      static const char *ops[] = {"read", "write", "exec"};
      ULONG_PTR op = ep->ExceptionRecord->ExceptionInformation[0];
      fa = (uintptr_t)ep->ExceptionRecord->ExceptionInformation[1];
      have_fa = 1;
      what = ops[op == 1 ? 1 : (op == 8 ? 2 : 0)];
      break;
    }
    case EXCEPTION_ILLEGAL_INSTRUCTION:
      what = "illegal instruction";
      break;
    case EXCEPTION_INT_DIVIDE_BY_ZERO:
      what = "integer divide by zero";
      break;
    case EXCEPTION_FLT_DIVIDE_BY_ZERO:
      what = "float divide by zero";
      break;
    case EXCEPTION_STACK_OVERFLOW:
      what = "stack overflow";
      break;
  }
  fprintf(stderr, "wbox-linux: fatal host exception %#lx (%s) at rip=%p\n",
          code, what, (void *)ctx->Rip);
  m = g_machine;  // thread-local of the crashing thread
  if (m && m->system) {
    uint64_t grip = m->ip;
    fprintf(stderr,
            "wbox-linux: guest pid=%d rip=%#llx in %s (code [%#llx,+%#lx))"
            " => guest+%#llx\n",
            m->system->pid, (unsigned long long)grip,
            m->system->elf.execfn ? m->system->elf.execfn
                                  : "(unknown guest)",
            (unsigned long long)m->system->codestart,
            (unsigned long)m->system->codesize,
            (unsigned long long)(grip - (uint64_t)m->system->codestart));
  } else {
    fprintf(stderr, "wbox-linux: fault outside guest context (host code)\n");
  }
  if (have_fa) {
    uintptr_t base = WboxMemWindowBase();
    uintptr_t lim = WboxMemLimit();
    if (base && fa >= base && fa < lim) {
      fprintf(stderr, "wbox-linux: guest fault address %#llx (host %p)\n",
              (unsigned long long)(fa - (uint64_t)base), (void *)fa);
    } else {
      fprintf(stderr, "wbox-linux: fault address %p (outside guest window)\n",
              (void *)fa);
    }
  }
  if (getenv("WBOX_DEBUG")) {
    fprintf(stderr,
            "wbox-linux: host rax=%p rbx=%p rcx=%p rdx=%p rsi=%p rdi=%p\n"
            "wbox-linux:      rbp=%p rsp=%p r8=%p r9=%p r10=%p r11=%p\n"
            "wbox-linux:      r12=%p r13=%p r14=%p r15=%p eflags=%#lx\n"
            "wbox-linux: window [%p,%p) skew=%#llx\n",
            (void *)ctx->Rax, (void *)ctx->Rbx, (void *)ctx->Rcx,
            (void *)ctx->Rdx, (void *)ctx->Rsi, (void *)ctx->Rdi,
            (void *)ctx->Rbp, (void *)ctx->Rsp, (void *)ctx->R8,
            (void *)ctx->R9, (void *)ctx->R10, (void *)ctx->R11,
            (void *)ctx->R12, (void *)ctx->R13, (void *)ctx->R14,
            (void *)ctx->R15, (unsigned long)ctx->EFlags,
            (void *)WboxMemWindowBase(), (void *)WboxMemLimit(),
            (unsigned long long)kSkew);
  }
}

static LONG WINAPI WboxVeh(EXCEPTION_POINTERS *ep) {
  DWORD code = ep->ExceptionRecord->ExceptionCode;
  struct Machine *m;
  siginfo_t si;
  int sig;
  switch (code) {
    case EXCEPTION_ACCESS_VIOLATION:
    case EXCEPTION_STACK_OVERFLOW:
      sig = SIGSEGV;
      break;
    case EXCEPTION_ILLEGAL_INSTRUCTION:
      sig = SIGILL;
      break;
    case EXCEPTION_INT_DIVIDE_BY_ZERO:
    case EXCEPTION_INT_OVERFLOW:
      sig = SIGFPE;
      break;
    case EXCEPTION_FLT_DIVIDE_BY_ZERO:
      sig = SIGFPE;
      break;
    default:
      return EXCEPTION_CONTINUE_SEARCH;
  }
  // Guest fault redirect: mirror the POSIX SA_SIGINFO path in blink.c
  // (OnFatalSystemSignal). A fault raised while guest code runs (canhalt)
  // is a GUEST page fault, not a host crash: forge the siginfo POSIX would
  // have delivered and siglongjmp back to the machine loop, which routes
  // it through HandleFatalSystemSignal -> DeliverSignalToUser. Without
  // this, every guest segfault killed the whole wine process.
  m = g_machine;  // thread-local of the crashing thread
  if (m && m->canhalt) {
    memset(&si, 0, sizeof(si));
    si.si_signo = sig;
    if (sig == SIGSEGV) {
      uintptr_t fa =
          (uintptr_t)ep->ExceptionRecord->ExceptionInformation[1];
      MEMORY_BASIC_INFORMATION mbi;
      si.si_addr = (void *)fa;
      // committed/reserved-but-denied is a protection fault (ACCERR);
      // never-reserved is MAPERR, like the host kernel would report.
      si.si_code =
          (VirtualQuery((LPCVOID)fa, &mbi, sizeof(mbi)) &&
           mbi.State != MEM_FREE)
              ? SEGV_ACCERR
              : SEGV_MAPERR;
    } else if (sig == SIGILL) {
      si.si_addr = (void *)ep->ExceptionRecord->ExceptionAddress;
      si.si_code = ILL_ILLOPC;
    } else {
      si.si_addr = (void *)ep->ExceptionRecord->ExceptionAddress;
      si.si_code = (code == EXCEPTION_INT_DIVIDE_BY_ZERO) ? FPE_INTDIV
                                                          : FPE_FLTDIV;
    }
#ifndef DISABLE_JIT
    // JIT self-modifying-code tracking: unprotect the code page and let
    // the faulting write retry, exactly like the POSIX handler.
    if (IsSelfModifyingCodeSegfault(m, &si)) {
      return EXCEPTION_CONTINUE_EXECUTION;
    }
#endif
    g_siginfo = si;
    siglongjmp(m->onhalt, kMachineFatalSystemSignal);
    // not reached
  }
  // fault in host code (or no live machine): diagnose + die
  WboxVehDiagnose(ep);
  ExitProcess(128 + sig);
  return EXCEPTION_CONTINUE_SEARCH;
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
