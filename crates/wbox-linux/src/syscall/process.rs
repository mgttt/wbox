//! 进程族 syscall：`fork` / `vfork` / `clone` / `execve` / `wait4` / `exit`。
//!
//! # 为什么是"快照式 fork"
//!
//! 真内核的 `fork` 是 COW + 父子并发调度。模拟器要做到并发，得把 `Machine`
//! 变成可调度的协程集合，并给每个阻塞点（`read`、`wait4`、futex、poll）都
//! 实现让出与唤醒——那是另一个量级的工程，也会重写掉现在这套直白的
//! 取指-译码-执行主循环。
//!
//! 被取代的 blink 当年选的是另一条路，这里照抄：
//!
//! 1. `fork` 时**克隆整个 guest 地址空间**（稀疏页表按值复制，见 `mem.rs`）
//!    与 CPU 状态，子进程的 `rax` 置 0；
//! 2. **先把子进程完整跑到退出**，记下退出状态；
//! 3. 再回到父进程，`fork` 返回子进程 pid；
//! 4. `wait4` 直接从"已退出子进程"表里取账。
//!
//! ## 这样能跑什么、不能跑什么
//!
//! 能跑的是**顺序**的多进程模式，也就是 shell 里绝大多数用法：
//! `a && b`、`a; b`、`$(cmd)`、`if cmd; then`、非并发的重定向。
//!
//! **不能**跑的是两端同时存活的并发结构：`a | b`（b 要读 a 边写边出的数据）、
//! `cmd &` 后父进程还要和它交互。前者在这里会看到管道提前 EOF 或死锁，
//! 后者会在 fork 点同步等到子进程结束。这是明确的已知限制，写在
//! `lib.rs` 的缺口清单和 `docs/rust-rewrite.md` 里，不是静默降级：
//! 需要真并发的场景会表现为挂住或读到空数据，而不是悄悄给出错答案。
//!
//! ## 递归深度
//!
//! 子进程跑在**宿主的调用栈**上（`fork` -> `run` -> `fork` -> ...），所以
//! 必须限深，否则一个 fork 炸弹会把宿主栈打爆成 SIGSEGV。超限返回 `EAGAIN`
//! ——正是 Linux 在达到进程数上限时给的 errno，guest 侧的 libc 认得。

use super::{guest_path, E2BIG, EAGAIN, ECHILD, EFAULT, EINTR, EINVAL, ENOSYS, ESRCH, PATH_MAX};
use crate::core::CoreState;
use crate::cpu::{Cpu, R11, RAX, RCX};
use crate::machine::{Exception, ExecResult, Machine};
use crate::mem::Mem;
use crate::proc;

/// `fork` 的嵌套上限。每一层占宿主一帧 `run()`，8 层足够任何真实 shell 脚本
/// （`a && b && c` 是同一层的连续 fork，不是嵌套）。
const MAX_FORK_DEPTH: u32 = 8;
/// argv/envp 的条目数上限，防止 guest 递一个不终止的指针数组把我们读死。
const MAX_ARGS: usize = 4096;

// clone(2) flags，只列我们要判断的几个。
const CLONE_VM: u64 = 0x100;
const CLONE_THREAD: u64 = 0x10000;

/// 一个已经退出、等着被 `wait4` 收账的子进程。
#[derive(Debug, Clone, Copy)]
pub struct ZombieChild {
    pub pid: i32,
    /// `wait4` 的 `wstatus` 编码（正常退出 `code << 8`，被信号杀 `signo`）。
    pub status: i32,
}

/// 把一次子进程运行的结果编码成 `wstatus`。
fn wstatus(r: &ExecResult<i32>) -> i32 {
    match r {
        Ok(code) => (code & 0xff) << 8,
        Err(e) => {
            let signo = match e {
                Exception::Fault(_) => 11,          // SIGSEGV
                Exception::Undefined { .. } => 4,   // SIGILL
                Exception::DivideError { .. } => 8, // SIGFPE
                Exception::Breakpoint { .. } => 5,  // SIGTRAP
                Exception::Killed { signo } => *signo,
                // Exit 走 Ok 分支，不会到这里；兜底按 SIGKILL 记。
                Exception::Exit(_) => 9,
                // `Machine::step()` consumes this trap before a child result
                // can reach here; retain a fail-closed status for malformed
                // callers that return it directly.
                Exception::Syscall { .. } => 4,
            };
            signo & 0x7f
        }
    }
}

/// `fork` / `vfork`，以及不带 `CLONE_VM` 的 `clone`。
///
/// `ret_rip` 是 `syscall` 指令之后的地址：子进程要从那里以 `rax=0` 继续。
pub fn sys_fork(m: &mut Machine, child_stack: u64, ret_rip: u64) -> i64 {
    if m.os.fork_depth >= MAX_FORK_DEPTH {
        return -EAGAIN;
    }

    let fds = match m.os.fds.try_clone() {
        Ok(t) => t,
        Err(e) => return super::host_err(&e),
    };

    let pid = m.os.alloc_pid();
    let mut child = Machine {
        core: CoreState {
            cpu: m.cpu.clone(),
            mem: m.mem.clone(),
            trace: m.trace,
            max_insns: m.max_insns,
        },
        os: m.os.clone_for_fork(fds, pid),
    };
    // fork 的返回值：父得 pid，子得 0。syscall 指令的副作用（rcx/r11/rip）
    // 也要在子进程里补上，否则它会从 syscall 指令本身重新执行。
    child.cpu.regs[RAX] = 0;
    child.cpu.regs[RCX] = ret_rip;
    child.cpu.regs[R11] = child.cpu.flags.pack();
    child.cpu.rip = ret_rip;
    // 带栈的 clone（glibc 的 fork 传 0，pthread 才传栈）。
    if child_stack != 0 {
        child.cpu.set_rsp(child_stack);
    }

    let r = child.run(child.max_insns);
    if let Err(e) = &r {
        // 子进程崩了要说出来。不打的话父进程只看到一个非零 wstatus，
        // 排查时完全无从下手。
        eprintln!("wbox-linux: 子进程 pid={pid} 异常终止：{e}");
    }
    // 指令预算是**全局**的：子进程花掉的步数记到父进程头上，否则
    // fork 循环可以绕开 WBOX_MAX_INSNS 这个安全阀。
    m.cpu.icount = child.cpu.icount;
    m.os.zombies.push(ZombieChild {
        pid,
        status: wstatus(&r),
    });
    pid as i64
}

/// `clone(2)`。只支持"等价于 fork"的用法；线程（`CLONE_VM|CLONE_THREAD`）
/// 明确 `ENOSYS`，不假装成功——假装成功会让 guest 以为多了一个执行流。
pub fn sys_clone(m: &mut Machine, flags: u64, child_stack: u64, ret_rip: u64) -> i64 {
    if flags & (CLONE_VM | CLONE_THREAD) != 0 {
        if m.os.strace {
            eprintln!("wbox-linux: (sys) clone(flags={flags:#x}) 是线程创建，未实现 -> ENOSYS");
        }
        return -ENOSYS;
    }
    sys_fork(m, child_stack, ret_rip)
}

/// 从 guest 读一个 `char *[]`（NULL 结尾）。
fn read_argv(m: &Machine, ptr: u64) -> Result<Vec<Vec<u8>>, i64> {
    if ptr == 0 {
        return Ok(Vec::new());
    }
    let ptrs = m.mem.read_ptr_array(ptr, MAX_ARGS).map_err(|_| -EFAULT)?;
    if ptrs.len() >= MAX_ARGS {
        return Err(-E2BIG);
    }
    let mut out = Vec::with_capacity(ptrs.len());
    for p in ptrs {
        out.push(m.mem.read_cstr(p, PATH_MAX * 8).map_err(|_| -EFAULT)?);
    }
    Ok(out)
}

/// `execve`。
///
/// 返回 `Ok(())` 表示**已经换好了地址空间**，调用方（`dispatch`）必须直接
/// 返回、不要再改 `rax`/`rip`——新程序的入口就是刚设好的那个。
/// 返回 `Err(errno)` 表示失败，此时地址空间**一个字节都没动**：装载全程在
/// 一个临时 `Mem` 里进行，成功才整体换过去。这是 `execve` 的硬性要求，
/// 否则 `execvp` 沿 `PATH` 逐个试的时候第一次失败就把进程搞坏了。
pub fn sys_execve(m: &mut Machine, path: u64, argv_p: u64, envp_p: u64) -> Result<(), i64> {
    let prog = guest_path(m, path)?;
    let mut argv = read_argv(m, argv_p)?;
    let envp = read_argv(m, envp_p)?;
    // argv 为空时内核仍然会给新程序一个 argc=1（argv[0] = 路径）。
    if argv.is_empty() {
        argv.push(prog.as_bytes().to_vec());
    }

    let mut fresh = Mem::new();
    let program = proc::load_into(&mut fresh, &m.os.vfs, &prog, &argv, &envp).map_err(|e| {
        if m.os.strace {
            eprintln!("wbox-linux: (sys) execve('{prog}') 失败：{}", e.msg);
        }
        e.errno
    })?;

    // 到这里已经不可能失败了，可以动真格。
    fresh.brk = program.loaded.brk;
    fresh.brk_start = program.loaded.brk;
    m.mem = fresh;

    // execve 会重置整套线程状态：寄存器、标志、XMM、FS/GS 基址都归零。
    // icount 是**我们的**计数器而不是 guest 状态，跨 exec 继续累加，
    // 这样 WBOX_MAX_INSNS 依然是整条 exec 链的总预算。
    let icount = m.cpu.icount;
    m.cpu = Cpu::new();
    m.cpu.icount = icount;
    m.cpu.set_rsp(program.rsp);
    m.cpu.rip = program.loaded.entry;

    m.os.exe = m.os.vfs.guest_abs(&prog);
    m.os.fds.close_on_exec();
    // 信号处置的复位规则见 `Os::reset_caught_signal_handlers_for_exec`。
    m.os.reset_caught_signal_handlers_for_exec();
    // 子进程表跨 exec 保留（Linux 语义：exec 换的是镜像，不是进程）。
    m.os.tid_address = 0;
    Ok(())
}

/// `wait4` / `waitpid`。
///
/// 快照式 fork 下所有子进程在 `fork` 返回前就已经跑完了，所以这里永远
/// 不需要阻塞：要么表里有账可收，要么真的没有子进程（`ECHILD`）。
/// `WNOHANG` 因此也总能立刻给出结果。
pub fn sys_wait4(m: &mut Machine, pid: i32, wstatus_p: u64, options: i32) -> i64 {
    const WNOHANG: i32 = 1;
    const WUNTRACED: i32 = 2;
    const WCONTINUED: i32 = 8;
    if options & !(WNOHANG | WUNTRACED | WCONTINUED) != 0 {
        return -EINVAL;
    }

    // pid > 0 等指定子进程；<= 0 的三种形式（-1 任意、0 同进程组、
    // -pgid 指定组）在单进程组的模拟器里都等价于"任意子进程"。
    let idx = match pid {
        p if p > 0 => m.os.zombies.iter().position(|z| z.pid == p),
        _ => (!m.os.zombies.is_empty()).then_some(0),
    };
    let Some(idx) = idx else {
        // 指定了一个不是我们孩子的 pid，或者根本没有子进程：都是 ECHILD。
        return -ECHILD;
    };

    let z = m.os.zombies.remove(idx);
    if wstatus_p != 0 && m.mem.write_u32(wstatus_p, z.status as u32).is_err() {
        // 状态已经从表里摘掉了，但写不回去说明 guest 给了坏指针；
        // 放回去以免这次 wait 把账吞掉。
        m.os.zombies.insert(idx, z);
        return -EFAULT;
    }
    z.pid as i64
}

/// `kill` / `tgkill`。
///
/// 模拟器不投递信号，但**给自己**发致命信号必须真的结束进程——`abort()`
/// 走的就是 `tgkill(getpid(), gettid(), SIGABRT)`，静默成功会让 guest 从
/// `abort()` 里返回，之后的行为完全不可预测。
pub fn sys_kill(m: &mut Machine, pid: i32, signo: i32) -> ExecResult<i64> {
    if !(0..=64).contains(&signo) {
        return Ok(-EINVAL);
    }
    if signo == 0 {
        // 只探测存在性。
        return Ok(if pid == m.os.pid || pid <= 0 {
            0
        } else {
            -ESRCH
        });
    }
    if pid == m.os.pid || pid == 0 || pid == -1 {
        use crate::syscall::signal::{self, Disposition};
        // **被屏蔽的信号不投递，只挂起**。这是 POSIX 语义，也是
        // "屏蔽 + signalfd" 这套写法能成立的前提——少了这一步，
        // `kill(getpid(), SIGUSR1)` 会把自己打死，而调用方本意是让
        // signalfd 收到它。
        let blocked = signal::sigset_bit(signo) & !signal::unmaskable() & m.os.sig_blocked != 0;
        if blocked {
            let me = m.os.pid;
            m.os.raise_signal(signo, signal::SI_USER, me);
            return Ok(0);
        }
        // 没被屏蔽：这一刻就按处置决定命运。
        //
        // `SIG_IGN`（以及默认动作就是忽略的那几个）**当场丢掉**，不进
        // pending——留着的话 `sigpending()` 会一直报它，而真实内核在这一刻
        // 就把它扔了；后来某次 `sigprocmask(SIG_BLOCK)` 再解除时还会诈尸。
        match signal::disposition(m, signo) {
            Disposition::Ignore => Ok(0),
            Disposition::Terminate => Err(Exception::Killed { signo }),
            Disposition::Handle(_) => {
                // 挂起，由 syscall 边界的投递点构帧跳进 handler——也就是
                // 这条 kill 返回之前，与内核的可观测顺序一致。
                let me = m.os.pid;
                m.os.raise_signal(signo, signal::SI_USER, me);
                Ok(0)
            }
        }
    } else {
        // 别的进程：快照式 fork 下子进程早已退出，没有活的目标。
        Ok(-ESRCH)
    }
}

/// `pause` / `sigsuspend`：等到有信号可投递为止，然后回 `EINTR`。
///
/// `sigsuspend` 多一步：先把屏蔽字换成调用方给的那份，醒来再换回去
/// （`mask` 是 `(set_ptr, sigsetsize)`）。构帧发生在 syscall 边界的投递点，
/// 而屏蔽字要在**构帧之前**还原——不然 handler 会带着 sigsuspend 的临时
/// 屏蔽字跑，`SIG_UNBLOCK` 的那一半就白做了。
///
/// # 什么时候仍然按 SIGKILL 收场
///
/// 单线程模拟器里信号只有两个来源：guest 自己 `kill`，和 `ITIMER_REAL`
/// 到期。`pause()` 之后 guest 不再执行任何指令，所以**没有定时器武装时
/// 这个等待是确定的死锁**，不是"暂时等不到"。那时回 `EINTR` 会让
/// `for (;;) pause();` 变成满 CPU 空转（CI 上表现为几百秒后超时，比失败
/// 难查得多），所以照旧终止并把原因打到 stderr。
pub fn sys_pause(m: &mut Machine, mask: Option<(u64, u64)>) -> ExecResult<i64> {
    use crate::syscall::signal;

    let saved = m.os.sig_blocked;
    if let Some((set, size)) = mask {
        if size != 8 {
            return Ok(-EINVAL);
        }
        match m.mem.read_u64(set) {
            Ok(v) => m.os.sig_blocked = v & !signal::unmaskable(),
            Err(_) => return Ok(-EFAULT),
        }
    }
    let restore = |m: &mut Machine| {
        if mask.is_some() {
            m.os.sig_blocked = saved;
        }
    };
    loop {
        if signal::has_deliverable(m) {
            restore(m);
            return Ok(-EINTR);
        }
        if m.os.alarm_deadline_ns == 0 {
            restore(m);
            eprintln!(
                "wbox-linux: pid={} 调用了 pause()/sigsuspend()，当前没有武装任何定时器，\
                 单线程模拟器里不会再有信号到来，该等待永远不会返回；按 SIGKILL 终止。",
                m.os.pid
            );
            return Err(Exception::Killed { signo: 9 });
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// `nanosleep` / `clock_nanosleep`。
///
/// 这个可以**真的**实现：模拟器是单线程的，睡就是宿主线程睡，时间也真的
/// 过去了，`clock_gettime` 前后的差值是对的。唯一做不到的是被信号打断
/// （`EINTR`），因为信号本来就不投递。
pub fn sys_nanosleep(m: &mut Machine, req: u64, rem: u64) -> i64 {
    let (secs, nanos) = match (m.mem.read_u64(req), m.mem.read_u64(req + 8)) {
        (Ok(s), Ok(n)) => (s, n),
        _ => return -EFAULT,
    };
    if nanos >= 1_000_000_000 {
        return -EINVAL;
    }
    // 上限一小时：guest 传个荒谬的值不该把整条 CI 挂住。
    let total = std::time::Duration::new(secs.min(3600), nanos as u32);
    // **分片睡**而不是一次睡满：定时器到期要能把这一觉打断。一次 `sleep`
    // 睡满的话，`setitimer(100ms)` + `nanosleep(2s)` 会老老实实睡够 2 秒，
    // 而 guest 等的是 100ms 后的 EINTR。
    let slice = std::time::Duration::from_millis(1);
    let start = std::time::Instant::now();
    while start.elapsed() < total {
        if crate::syscall::signal::has_deliverable(m) {
            // 被打断：`rem` 要写回真实剩余时间，guest 拿它续睡。
            let left = total.saturating_sub(start.elapsed());
            if rem != 0
                && (m.mem.write_u64(rem, left.as_secs()).is_err()
                    || m.mem
                        .write_u64(rem + 8, left.subsec_nanos() as u64)
                        .is_err())
            {
                return -EFAULT;
            }
            return -EINTR;
        }
        std::thread::sleep(slice.min(total.saturating_sub(start.elapsed())));
    }
    // 睡满了，剩余时间是 0；`rem` 只在被打断时才有意义，但 Linux 允许
    // 正常返回时也不动它——这里显式清零更保险（guest 可能不初始化）。
    if rem != 0 {
        let _ = m.mem.write_u64(rem, 0);
        let _ = m.mem.write_u64(rem + 8, 0);
    }
    0
}
