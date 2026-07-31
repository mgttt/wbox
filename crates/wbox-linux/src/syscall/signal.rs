//! 信号的**投递**：处置表、信号帧、`rt_sigreturn`。
//!
//! # 为什么这一层必须有
//!
//! 在这之前引擎只做了"记 pending、给 signalfd 消费"的那一半。那一半是完整
//! 可用的（屏蔽 + signalfd 是现代服务端的标准写法），但凡是**真的要跳进
//! guest 的处理函数**的东西——`alarm` + `pause`、`SIGINT` 里做清理、
//! `sigaction` 装 handler 后 `kill(getpid(), ...)`——都还落空。落空的方式
//! 还很难查：`pause()` 直接被引擎判死刑，用例只看到"killed by signal 9"。
//!
//! # 做法：老老实实构一个和内核同布局的 `rt_sigframe`
//!
//! 本可以偷懒——反正内核这一侧是我们自己，把被中断的上下文存在引擎里，
//! `rt_sigreturn` 时再取回来就行，guest 完全看不出差别……**除非它去看
//! `ucontext`**。带 `SA_SIGINFO` 的 handler 第三个参数就是它，libunwind、
//! 崩溃采集器、Go 的运行时都会读。存在引擎里等于给了一个"看着像、其实是
//! 垃圾"的指针，比不支持更坏。
//!
//! 所以帧按 x86-64 内核的真实布局摆：
//!
//! ```text
//!   frame+0    pretcode（= sa_restorer，handler `ret` 回到它）
//!   frame+8    struct ucontext
//!                +0    uc_flags
//!                +8    uc_link
//!                +16   uc_stack（ss_sp / ss_flags / ss_size）
//!                +40   uc_mcontext（struct sigcontext_64，256 字节）
//!                +296  uc_sigmask（被中断时的屏蔽字）
//!   frame+312  struct siginfo（128 字节）
//! ```
//!
//! handler 入口时 `rsp == frame`，于是它 `ret` 之后 `rsp == frame+8`，正好
//! 指向 `ucontext`——这就是内核 `rt_sigreturn` 里那句
//! `frame = (struct rt_sigframe *)(regs->sp - sizeof(long))` 的由来。照抄这
//! 个约定，musl/glibc 的 restorer 一个字节都不用改。
//!
//! # 已知缺口（写在这里，不留给读者去猜）
//!
//! - **`sigaltstack` 没做**：`SA_ONSTACK` 被接受但仍用当前栈。栈溢出时
//!   的 SIGSEGV handler 因此不可靠。
//! - **`SA_RESTART` 没做**：被打断的 syscall 不重启。当前投递点在 syscall
//!   **返回之后**，被打断的只有 `pause`/`nanosleep`，而这两个即使带
//!   `SA_RESTART` 也是返回 `EINTR`（内核同此），所以暂时观察不到差异。
//! - **投递点只有 syscall 边界与可中断等待**：纯计算循环里的信号要等到下
//!   一次 syscall 才被看见。单线程模拟器里信号只能来自自己或定时器，这一
//!   条不影响可观测行为。
//! - **不保存 x87/SSE 状态**：`uc_mcontext.fpstate` 恒为 0。

use crate::cpu::{R10, R11, R8, R9, RAX, RBP, RBX, RCX, RDI, RDX, RSI, RSP};
use crate::machine::{Exception, Machine};

/// `SIG_DFL`：默认动作。
pub const SIG_DFL: u64 = 0;
/// `SIG_IGN`：忽略。
pub const SIG_IGN: u64 = 1;

/// `SA_SIGINFO`：handler 取三个参数。本引擎无论有没有它都把 siginfo/ucontext
/// 指针放进 rsi/rdx（内核同此），所以这里只作为常量记录，不参与分支。
#[allow(dead_code)]
pub const SA_SIGINFO: u64 = 0x0000_0004;
pub const SA_RESTORER: u64 = 0x0400_0000;
pub const SA_NODEFER: u64 = 0x4000_0000;
pub const SA_RESETHAND: u64 = 0x8000_0000;

pub const SIGKILL: i32 = 9;
pub const SIGSEGV: i32 = 11;
pub const SIGALRM: i32 = 14;
pub const SIGSTOP: i32 = 19;

/// `SI_USER`：来自 `kill`。
pub const SI_USER: i32 = 0;
/// `SI_KERNEL`：内核产生（定时器等）。
pub const SI_KERNEL: i32 = 0x80;

/// 默认动作是**忽略**的信号。其余标准信号的默认动作都是终止（本引擎不区分
/// core dump，退出码同为 128+signo）。
const DEFAULT_IGNORED: [i32; 4] = [
    17, // SIGCHLD
    18, // SIGCONT
    23, // SIGURG
    28, // SIGWINCH
];

/// `struct sigaction` 的引擎侧表示。字段顺序与内核 ABI 一致，见
/// [`read_sigaction`]。
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct SigAction {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    /// handler 执行期间**额外**屏蔽的信号（`sa_mask`）。
    pub mask: u64,
}

/// 信号号 → sigset 位。越界返回 0（调用方据此静默忽略，与内核一致）。
pub fn sigset_bit(signo: i32) -> u64 {
    if (1..=64).contains(&signo) {
        1u64 << (signo - 1)
    } else {
        0
    }
}

/// 不可屏蔽的两个信号。
pub fn unmaskable() -> u64 {
    sigset_bit(SIGKILL) | sigset_bit(SIGSTOP)
}

/// 某个信号当下的处置。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Disposition {
    /// 丢弃（`SIG_IGN`，或默认动作就是忽略）。
    Ignore,
    /// 终止进程。
    Terminate,
    /// 调 guest 的处理函数。
    Handle(SigAction),
}

/// `struct sigaction` 在 x86-64 上的字节数（handler/flags/restorer/mask）。
pub const SIGACTION_SIZE: u64 = 32;

/// 从 guest 内存读一个 `struct sigaction`。
pub fn read_sigaction(m: &Machine, at: u64) -> Result<SigAction, i64> {
    let f = |off: u64| {
        m.mem
            .read_u64(at + off)
            .map_err(|_| -crate::syscall::EFAULT)
    };
    Ok(SigAction {
        handler: f(0)?,
        flags: f(8)?,
        restorer: f(16)?,
        mask: f(24)?,
    })
}

/// 往 guest 内存写一个 `struct sigaction`。
///
/// **四个字段都要写**。早先只写 handler 那 8 字节，`sigaction(sig, NULL, &old)`
/// 取回来的 `old.sa_flags`/`sa_mask` 是调用方栈上的残值——用「先取旧的、
/// 改一位、再装回去」这套常见写法的程序会把垃圾 flags 装进内核。
pub fn write_sigaction(m: &mut Machine, at: u64, sa: &SigAction) -> Result<(), i64> {
    let mut b = [0u8; SIGACTION_SIZE as usize];
    b[0..8].copy_from_slice(&sa.handler.to_le_bytes());
    b[8..16].copy_from_slice(&sa.flags.to_le_bytes());
    b[16..24].copy_from_slice(&sa.restorer.to_le_bytes());
    b[24..32].copy_from_slice(&sa.mask.to_le_bytes());
    m.mem.write(at, &b).map_err(|_| -crate::syscall::EFAULT)
}

/// 结算定时器后，挑出**当下可投递**的信号在 pending 里的下标。
///
/// 按信号号从小到大——内核就是这个顺序。
fn pick_deliverable(m: &Machine) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, (s, _, _)) in m.os.sig_pending.iter().enumerate() {
        if sigset_bit(*s) & m.os.sig_blocked != 0 {
            continue;
        }
        if best.is_none_or(|b| *s < m.os.sig_pending[b].0) {
            best = Some(i);
        }
    }
    best
}

/// 现在有没有信号能被投递（`pause`/`nanosleep` 判断该不该醒）。
pub fn has_deliverable(m: &mut Machine) -> bool {
    m.os.settle_alarm();
    pick_deliverable(m).is_some()
}

/// 某个信号当下的处置。
pub fn disposition(m: &Machine, signo: i32) -> Disposition {
    let sa = m.os.sig_actions[signo.clamp(0, 64) as usize];
    match sa.handler {
        SIG_IGN => Disposition::Ignore,
        SIG_DFL => {
            if DEFAULT_IGNORED.contains(&signo) {
                Disposition::Ignore
            } else {
                Disposition::Terminate
            }
        }
        _ => Disposition::Handle(sa),
    }
}

/// 在 syscall 边界投递一个挂起信号。
///
/// 一次只投一个——内核也是这样，剩下的等 `rt_sigreturn` 之后的下一个边界。
/// 处置是 `Ignore` 的直接丢掉并继续看下一个（**丢掉**而不是留在 pending：
/// 留着的话 `sigpending()` 会一直报它，而真实内核在这一刻就把它扔了）。
pub fn deliver_pending(m: &mut Machine) -> Result<(), Exception> {
    loop {
        m.os.settle_alarm();
        let Some(idx) = pick_deliverable(m) else {
            return Ok(());
        };
        let (signo, code, pid) = m.os.sig_pending.remove(idx);
        match disposition(m, signo) {
            Disposition::Ignore => continue,
            Disposition::Terminate => return Err(Exception::Killed { signo }),
            Disposition::Handle(sa) => return setup_frame(m, signo, code, pid, sa),
        }
    }
}

/// `struct sigcontext_64` 里 `rsp` 的偏移（相对 `uc_mcontext`）。
const MC_RSP: u64 = 120;
/// 同上，`rip`。
const MC_RIP: u64 = 128;
/// 同上，`eflags`。
const MC_FLAGS: u64 = 136;
/// `uc_mcontext` 相对 `ucontext` 起点的偏移。
const UC_MCONTEXT: u64 = 40;
/// `uc_sigmask` 相对 `ucontext` 起点的偏移。
const UC_SIGMASK: u64 = 296;
/// `ucontext` 相对帧起点的偏移（前 8 字节是 pretcode）。
const FRAME_UCONTEXT: u64 = 8;
/// `siginfo` 相对帧起点的偏移。
const FRAME_SIGINFO: u64 = 312;
/// 整个 `rt_sigframe` 的大小。
const FRAME_SIZE: u64 = 440;
/// x86-64 ABI 的红区：`rsp` 以下 128 字节属于被中断的函数，不能踩。
const RED_ZONE: u64 = 128;

/// `sigcontext_64` 的通用寄存器排布（偏移 → 寄存器号）。
/// 顺序取自内核 `arch/x86/include/uapi/asm/sigcontext.h`。
const MC_GPRS: [(u64, usize); 15] = [
    (0, R8),
    (8, R9),
    (16, R10),
    (24, R11),
    (32, 12), // r12
    (40, 13),
    (48, 14),
    (56, 15),
    (64, RDI),
    (72, RSI),
    (80, RBP),
    (88, RBX),
    (96, RDX),
    (104, RAX),
    (112, RCX),
];

/// 构信号帧、改 rip，把控制权交给 guest 的 handler。
fn setup_frame(
    m: &mut Machine,
    signo: i32,
    code: i32,
    pid: i32,
    sa: SigAction,
) -> Result<(), Exception> {
    // x86-64 上内核要求 libc 提供 restorer（`ret` 之后要有人去调
    // `rt_sigreturn`）。没有就 `force_sigsegv`——照抄这个行为，而不是自己
    // 在 guest 内存里种一段蹦床：种蹦床要找一块可执行的页，那是凭空多出来
    // 的一份攻击面，而所有真实 libc 都会设 SA_RESTORER。
    if sa.flags & SA_RESTORER == 0 {
        return Err(Exception::Killed { signo: SIGSEGV });
    }

    let sp = m.cpu.regs[RSP].saturating_sub(RED_ZONE);
    // handler 入口的 rsp 必须与"刚被 call 进来"一致：rsp ≡ 8 (mod 16)。
    let frame = (sp.saturating_sub(FRAME_SIZE) & !15u64).wrapping_sub(8);

    let mut buf = [0u8; FRAME_SIZE as usize];
    let put = |b: &mut [u8], off: u64, v: u64| {
        let o = off as usize;
        b[o..o + 8].copy_from_slice(&v.to_le_bytes());
    };

    // pretcode：handler `ret` 回到 libc 的 restorer，它只做一件事——
    // `mov $15, %eax; syscall`（rt_sigreturn）。
    put(&mut buf, 0, sa.restorer);

    let uc = FRAME_UCONTEXT;
    let mc = uc + UC_MCONTEXT;
    for (off, reg) in MC_GPRS {
        put(&mut buf, mc + off, m.cpu.regs[reg]);
    }
    put(&mut buf, mc + MC_RSP, m.cpu.regs[RSP]);
    put(&mut buf, mc + MC_RIP, m.cpu.rip);
    put(&mut buf, mc + MC_FLAGS, m.cpu.flags.pack());
    // uc_sigmask 存**被中断时**的屏蔽字，`rt_sigreturn` 据此还原。
    put(&mut buf, uc + UC_SIGMASK, m.os.sig_blocked);

    // siginfo：只填 guest 真能用上的几个字段，其余留 0。编一堆猜出来的值
    // 只会让人误以为它们可信。
    let si = FRAME_SIGINFO as usize;
    buf[si..si + 4].copy_from_slice(&signo.to_le_bytes());
    buf[si + 8..si + 12].copy_from_slice(&code.to_le_bytes());
    buf[si + 16..si + 20].copy_from_slice(&pid.to_le_bytes());

    if m.mem.write(frame, &buf).is_err() {
        // 栈写不进去（爆栈或栈页没映射）——内核这时也是 force_sigsegv。
        return Err(Exception::Killed { signo: SIGSEGV });
    }

    // 进 handler 期间的屏蔽字：sa_mask，外加信号自己（除非 SA_NODEFER）。
    m.os.sig_blocked |= sa.mask;
    if sa.flags & SA_NODEFER == 0 {
        m.os.sig_blocked |= sigset_bit(signo);
    }
    m.os.sig_blocked &= !unmaskable();
    if sa.flags & SA_RESETHAND != 0 {
        m.os.sig_actions[signo as usize] = SigAction::default();
    }

    m.cpu.regs[RSP] = frame;
    m.cpu.regs[RDI] = signo as u64;
    // 不带 SA_SIGINFO 时 guest 也不会去读这两个参数，但内核照样设——
    // 少设的话，某些 handler 里的可变参数序言（读 al）会拿到脏值。
    m.cpu.regs[RSI] = frame + FRAME_SIGINFO;
    m.cpu.regs[RDX] = frame + FRAME_UCONTEXT;
    m.cpu.regs[RAX] = 0;
    // 内核进 handler 前清 DF：ABI 规定被调用方可以假定 DF=0。
    m.cpu.flags.df = false;
    m.cpu.rip = sa.handler;
    Ok(())
}

/// `rt_sigreturn`（syscall 15）。
///
/// **不能走分派表统一的"写 rax / 设 rip"收尾**：整套寄存器都要从帧里恢复，
/// 包括 rax（它是被中断的那个 syscall 的返回值）。所以和 `execve` 一样在
/// 分派前单独处理。
pub fn sys_rt_sigreturn(m: &mut Machine) -> Result<(), Exception> {
    // handler 的 `ret` 弹掉了 pretcode，此刻 rsp 正好指向 ucontext。
    let uc = m.cpu.regs[RSP];
    let mc = uc + UC_MCONTEXT;
    let mut vals = [0u64; 15];
    for (i, (off, _)) in MC_GPRS.iter().enumerate() {
        match m.mem.read_u64(mc + off) {
            Ok(v) => vals[i] = v,
            Err(_) => return Err(Exception::Killed { signo: SIGSEGV }),
        }
    }
    let (rsp, rip, flags, mask) = match (
        m.mem.read_u64(mc + MC_RSP),
        m.mem.read_u64(mc + MC_RIP),
        m.mem.read_u64(mc + MC_FLAGS),
        m.mem.read_u64(uc + UC_SIGMASK),
    ) {
        (Ok(a), Ok(b), Ok(c), Ok(d)) => (a, b, c, d),
        _ => return Err(Exception::Killed { signo: SIGSEGV }),
    };
    for (i, (_, reg)) in MC_GPRS.iter().enumerate() {
        m.cpu.regs[*reg] = vals[i];
    }
    m.cpu.regs[RSP] = rsp;
    m.cpu.rip = rip;
    m.cpu.flags.unpack(flags);
    m.os.sig_blocked = mask & !unmaskable();
    // 还原屏蔽字之后可能又有信号能投了（handler 里 kill 自己，或期间到期的
    // 定时器）。内核在返回用户态前会再走一遍投递，这里同样。
    deliver_pending(m)
}
