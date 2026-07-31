//! Linux x86-64 syscall 模拟。
//!
//! 约定：每个 `sys_*` 返回 `i64`，负值是 `-errno`（Linux 内核的 ABI）。
//! guest 侧的 libc 会自己把负值翻成 `errno`。
//!
//! **没实现的 syscall 一律返回 `-ENOSYS` 并在 `WBOX_STRACE=1` 下打印**，
//! 绝不假装成功——假装成功会让 guest 在离故障点很远的地方以莫名其妙的方式坏掉。

pub mod fs;
pub mod net;
pub mod process;

use crate::cpu::{R10, R11, R8, R9, RAX, RCX, RDI, RDX, RSI};
use crate::machine::{Exception, ExecResult, Machine};
use crate::mem::{PAGE_MASK, PROT_EXEC, PROT_READ, PROT_WRITE};
use fs::{Fd, FdKind, FdTable, Vfs};
use std::cell::Cell;
#[cfg(windows)]
use std::cell::RefCell;
#[cfg(windows)]
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::rc::Rc;

// ---------------------------------------------------------------- errno
pub const EPERM: i64 = 1;
pub const EIO: i64 = 5;
pub const ENOENT: i64 = 2;
pub const ESRCH: i64 = 3;
pub const E2BIG: i64 = 7;
pub const ENOEXEC: i64 = 8;
pub const EBADF: i64 = 9;
pub const ECHILD: i64 = 10;
pub const EAGAIN: i64 = 11;
pub const ENOMEM: i64 = 12;
pub const EACCES: i64 = 13;
pub const EFAULT: i64 = 14;
pub const EEXIST: i64 = 17;
pub const ENOTDIR: i64 = 20;
pub const EISDIR: i64 = 21;
pub const EINVAL: i64 = 22;
pub const EMFILE: i64 = 24;
pub const ENOTTY: i64 = 25;
pub const ESPIPE: i64 = 29;
pub const EPIPE: i64 = 32;
pub const ENOSPC: i64 = 28;
pub const ENAMETOOLONG: i64 = 36;
pub const ENOSYS: i64 = 38;
pub const ENOTEMPTY: i64 = 39;
pub const ELOOP: i64 = 40;
pub const EROFS: i64 = 30;
pub const ENODEV: i64 = 19;
pub const EPROTONOSUPPORT: i64 = 93;
pub const ESOCKTNOSUPPORT: i64 = 94;
pub const EOPNOTSUPP: i64 = 95;
pub const EAFNOSUPPORT: i64 = 97;
pub const EADDRINUSE: i64 = 98;
pub const ENOTSOCK: i64 = 88;
pub const EDESTADDRREQ: i64 = 89;
pub const ECONNREFUSED: i64 = 111;
pub const EISCONN: i64 = 106;
pub const ENOTCONN: i64 = 107;
pub const EALREADY: i64 = 114;
pub const EINPROGRESS: i64 = 115;

// ------------------------------------------------------------ open flags
const O_ACCMODE: i32 = 3;
const O_RDONLY: i32 = 0;
const O_WRONLY: i32 = 1;
const O_RDWR: i32 = 2;
const O_CREAT: i32 = 0o100;
const O_EXCL: i32 = 0o200;
const O_TRUNC: i32 = 0o1000;
const O_APPEND: i32 = 0o2000;
const O_NONBLOCK: i32 = 0o4000;
const O_DIRECTORY: i32 = 0o200000;
const O_CLOEXEC: i32 = 0o2000000;
/// `__O_TMPFILE`。glibc 的 `O_TMPFILE` 是它按位或上 `O_DIRECTORY`，
/// 所以判定要看这一位而不是整个常量。
const O_TMPFILE: i32 = 0o20000000;

const AT_FDCWD: i32 = -100;
/// `newfstatat` 的 `AT_EMPTY_PATH`：路径为空时对 fd 本身取状态。
const AT_EMPTY_PATH: i32 = 0x1000;
const AT_SYMLINK_NOFOLLOW: i32 = 0x100;
const AT_REMOVEDIR: i32 = 0x200;

const PATH_MAX: usize = 4096;

/// guest 侧可见的 OS 状态。
pub struct Os {
    pub fds: FdTable,
    pub vfs: Vfs,
    /// guest 进程号。初始进程是 1（容器里的 init）。
    pub pid: i32,
    /// 父进程号。初始进程报 0（Linux 上 init 的 ppid 就是 0）。
    pub ppid: i32,
    /// pid 分配器。**整棵进程树共享一个计数器**，所以要 `Rc<Cell<..>>`：
    /// fork 出来的子进程再 fork 时不能发出和别人重复的 pid。
    pid_alloc: Rc<Cell<i32>>,
    /// 已退出、等着 `wait4` 收账的子进程。见 `process.rs` 的快照式 fork 说明。
    pub zombies: Vec<process::ZombieChild>,
    /// 当前 fork 嵌套深度。子进程跑在宿主调用栈上，必须限深。
    pub fork_depth: u32,
    /// `O_TMPFILE` 的名字计数器（见 `open_tmpfile`）。
    tmpfile_seq: u64,
    /// 正在运行的镜像的 **guest 绝对路径**，`readlink("/proc/self/exe")` 要回它。
    /// 启动时与每次 `execve` 成功后更新。空串表示还没装载。
    pub exe: String,
    /// `set_tid_address` 记下的地址，线程退出时要清零（当前单线程，仅存不用）。
    pub tid_address: u64,
    /// `WBOX_STRACE=1`：把每次 syscall 打到 stderr。
    pub strace: bool,
    /// `uname` 报的内核版本。
    pub release: String,
    /// `umask` 的当前值。新建 guest 文件的虚拟 Unix mode 会应用它。
    pub umask: u32,
    /// Windows 不保存 Unix permission bits；按稳定文件 identity 维护 guest
    /// 自己的权限视图。放在共享表里，使 snapshot fork 创建或观察到的文件
    /// 在父子进程间保持同一份文件系统元数据。
    #[cfg(windows)]
    file_modes: Rc<RefCell<HashMap<(u64, u64), u32>>>,
    /// 信号屏蔽字（位 0 = 信号 1）。
    ///
    /// # 为什么"只记 pending、不投递"仍然有用
    ///
    /// 真正的信号投递要打断执行流（构帧、改 rip、`sigreturn`），那是另一件
    /// 大工程。但**被屏蔽的信号本来就不投递**——它只是挂在 pending 上，等
    /// `signalfd`/`sigwait`/解除屏蔽时才被消费。而"屏蔽 + signalfd"恰恰是
    /// 现代服务端处理信号的标准写法（避免 handler 里的异步安全约束）。
    /// 所以这一半单独做出来是完整可用的，不是半成品。
    pub sig_blocked: u64,
    /// 已挂起、还没被消费的信号。元素是 `(signo, si_code, si_pid)`。
    pub sig_pending: Vec<(i32, i32, i32)>,
    /// `sigaction` 记下的处理函数地址（下标即信号号）。目前只存不调用。
    pub sig_handlers: [u64; 65],
    /// `setitimer(ITIMER_REAL)` 的到期时刻（纳秒，0 = 未武装）。
    /// 与 timerfd 同样是**惰性结算**的，见 `fs::TimerFd` 的说明。
    pub alarm_deadline_ns: u64,
    pub alarm_interval_ns: u64,
    /// 活着的**文件映射**。见 [`FileMap`]。
    pub file_maps: Vec<FileMap>,
}

/// 一段文件映射。
///
/// # 为什么要单独记账
///
/// 引擎的内存是自己的稀疏页表，不是宿主的 `mmap`——所以"写到映射里"只是改了
/// 页表，宿主文件毫不知情。`MAP_SHARED` 承诺的"写入落到文件"于是完全没有兑现
/// （`t_mmap` 的 file-shared-* 全组红）。
///
/// 记下 (地址, 长度, 文件, 偏移) 之后，就能在**可观测的时刻**把内容刷回去。
/// 刷回点选在 `munmap`/`msync`/`mremap`/进程退出——POSIX 只保证这些点之后
/// 数据可见，而 guest 也只能在这些点之后去读文件。每次 guest 写内存都同步
/// 回盘既做不到（没有写钩子）也没必要。
///
/// 文件句柄是 `try_clone` 来的**独立副本**：guest 关掉自己那个 fd 之后映射
/// 依然有效，这是 POSIX 明确规定的（`mremap/file-private-move-after-close`
/// 专门测了这条）。
pub struct FileMap {
    pub base: u64,
    pub len: u64,
    pub file: std::fs::File,
    pub offset: u64,
    /// `MAP_SHARED`：写入要回盘。`MAP_PRIVATE` 只是"从文件初始化"，不回写。
    pub shared: bool,
}

impl Default for Os {
    fn default() -> Self {
        Self::new()
    }
}

impl Os {
    pub fn new() -> Self {
        Os {
            fds: FdTable::new(),
            vfs: Vfs::from_env(),
            pid: 1,
            ppid: 0,
            pid_alloc: Rc::new(Cell::new(2)),
            zombies: Vec::new(),
            fork_depth: 0,
            tmpfile_seq: 0,
            exe: String::new(),
            tid_address: 0,
            strace: std::env::var_os("WBOX_STRACE").is_some_and(|v| v != "0"),
            // 报一个足够新的版本：glibc 会检查内核版本下限，报太老会直接
            // "FATAL: kernel too old" 退出。
            release: "6.1.0-wbox".to_string(),
            umask: 0o022,
            #[cfg(windows)]
            file_modes: Rc::new(RefCell::new(HashMap::new())),
            sig_blocked: 0,
            sig_pending: Vec::new(),
            sig_handlers: [0; 65],
            alarm_deadline_ns: 0,
            alarm_interval_ns: 0,
            file_maps: Vec::new(),
        }
    }

    /// 把到当前时刻为止的 `ITIMER_REAL` 到期结算成 pending 的 SIGALRM。
    pub fn settle_alarm(&mut self) {
        const SIGALRM: i32 = 14;
        const SI_KERNEL: i32 = 0x80;
        if self.alarm_deadline_ns == 0 || now_ns() < self.alarm_deadline_ns {
            return;
        }
        self.alarm_deadline_ns = if self.alarm_interval_ns == 0 {
            0
        } else {
            now_ns() + self.alarm_interval_ns
        };
        self.raise_signal(SIGALRM, SI_KERNEL, 0);
    }

    /// 记一个挂起信号。**同号只挂一次**——标准信号不排队，这是 POSIX 语义，
    /// 也是 `signalfd` 用例里连发两次 SIGUSR2 只读到一条的原因。
    pub fn raise_signal(&mut self, signo: i32, code: i32, pid: i32) {
        if !self.sig_pending.iter().any(|(s, _, _)| *s == signo) {
            self.sig_pending.push((signo, code, pid));
        }
    }

    /// 分配一个新 pid（整棵进程树共享计数器）。
    pub fn alloc_pid(&self) -> i32 {
        let p = self.pid_alloc.get();
        self.pid_alloc.set(p.wrapping_add(1).max(2));
        p
    }

    /// 造出 fork 后子进程的 `Os`。
    ///
    /// fd 表由调用方 `try_clone` 好传进来（那一步会失败，得先做）。
    /// 子进程**不继承**父进程的僵尸表——那些是父进程的孩子，不是它的。
    pub fn clone_for_fork(&self, fds: FdTable, pid: i32) -> Os {
        Os {
            fds,
            vfs: self.vfs.clone(),
            pid,
            ppid: self.pid,
            sig_blocked: self.sig_blocked,
            // **pending 不继承**：POSIX 规定 fork 出来的子进程 pending 集合为空。
            sig_pending: Vec::new(),
            sig_handlers: self.sig_handlers,
            alarm_deadline_ns: 0,
            alarm_interval_ns: 0,
            // 快照式 fork：子进程有自己的页表副本，映射记账也各算各的。
            // "MAP_SHARED 跨进程共享"那一层没做（基线 A 组），如实不继承——
            // 继承一份句柄反而会让父子各自把自己的页写回同一个文件，
            // 表现成互相覆盖，比不做更难查。
            file_maps: Vec::new(),
            pid_alloc: Rc::clone(&self.pid_alloc),
            zombies: Vec::new(),
            fork_depth: self.fork_depth + 1,
            tmpfile_seq: 0,
            exe: self.exe.clone(),
            tid_address: self.tid_address,
            strace: self.strace,
            release: self.release.clone(),
            umask: self.umask,
            #[cfg(windows)]
            file_modes: Rc::clone(&self.file_modes),
        }
    }
}

/// 把宿主 `io::Error` 翻成 `-errno`。
pub fn host_err(e: &std::io::Error) -> i64 {
    use std::io::ErrorKind as K;
    -match e.kind() {
        K::NotFound => ENOENT,
        K::PermissionDenied => EACCES,
        K::AlreadyExists => EEXIST,
        K::InvalidInput => EINVAL,
        K::BrokenPipe => EPIPE,
        K::WouldBlock => EAGAIN,
        K::IsADirectory => EISDIR,
        K::NotADirectory => ENOTDIR,
        K::DirectoryNotEmpty => ENOTEMPTY,
        _ => {
            // std 没有映射的错误：Unix 上还能拿到 raw errno，别丢掉信息。
            #[cfg(unix)]
            {
                if let Some(n) = e.raw_os_error() {
                    return -(n as i64);
                }
            }
            EINVAL
        }
    }
}

/// `syscall` 指令的入口。`ret_rip` 是 syscall 指令之后的地址。
pub fn dispatch(m: &mut Machine, ret_rip: u64) -> ExecResult<()> {
    let nr = m.cpu.regs[RAX];
    // System V syscall ABI：参数在 rdi/rsi/rdx/r10/r8/r9
    let a = [
        m.cpu.regs[RDI],
        m.cpu.regs[RSI],
        m.cpu.regs[RDX],
        m.cpu.regs[R10],
        m.cpu.regs[R8],
        m.cpu.regs[R9],
    ];

    if m.os.strace {
        // `(sys)` 是沿用被取代的 blink 的子系统标签：驱动 blink 的脚本
        // （含 scripts/test-windows-product.ps1 的 WP.4S）按它筛 syscall 行。
        eprintln!(
            "wbox-linux: (sys) syscall {} ({:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x})",
            nr, a[0], a[1], a[2], a[3], a[4], a[5]
        );
    }

    // execve 成功时**不能**走后面统一的"写 rax / 设 rip"收尾：新程序的
    // 入口和栈已经设好了，再改就跳回旧镜像了。所以它在分派表之前单独处理。
    if nr == 59 {
        match process::sys_execve(m, a[0], a[1], a[2]) {
            Ok(()) => return Ok(()),
            Err(errno) => {
                m.cpu.regs[RAX] = errno as u64;
                m.cpu.regs[RCX] = ret_rip;
                m.cpu.regs[R11] = m.cpu.flags.pack();
                m.cpu.rip = ret_rip;
                return Ok(());
            }
        }
    }

    let ret = match nr {
        0 => sys_read(m, a[0] as i32, a[1], a[2]),
        1 => sys_write(m, a[0] as i32, a[1], a[2]),
        2 => sys_openat(m, AT_FDCWD, a[0], a[1] as i32, a[2] as u32),
        3 => sys_close(m, a[0] as i32),
        4 => sys_stat_path(m, AT_FDCWD, a[0], a[1], true),
        5 => sys_fstat(m, a[0] as i32, a[1]),
        6 => sys_stat_path(m, AT_FDCWD, a[0], a[1], false),
        8 => sys_lseek(m, a[0] as i32, a[1] as i64, a[2] as i32),
        9 => sys_mmap(
            m,
            a[0],
            a[1],
            a[2] as i32,
            a[3] as i32,
            a[4] as i32,
            a[5] as i64,
        ),
        10 => sys_mprotect(m, a[0], a[1], a[2] as i32),
        11 => sys_munmap(m, a[0], a[1]),
        12 => sys_brk(m, a[0]),
        // rt_sigqueueinfo：没有队列可放，直接成功。13/14 见下方信号一节。
        131 => 0,
        16 => sys_ioctl(m, a[0] as i32, a[1], a[2]),
        17 => sys_pread(m, a[0] as i32, a[1], a[2], a[3] as i64),
        // preadv/pwritev（以及带 flags 的 v2 变体）。偏移是 pos_l|pos_h<<32，
        // 我们只支持 64 位宿主，pos_h 恒为 0，直接取 a[3]。
        295 | 327 => sys_preadv(m, a[0] as i32, a[1], a[2], a[3] as i64),
        296 | 328 => sys_pwritev(m, a[0] as i32, a[1], a[2], a[3] as i64),
        19 => sys_readv(m, a[0] as i32, a[1], a[2]),
        20 => sys_writev(m, a[0] as i32, a[1], a[2]),
        21 => sys_faccessat(m, AT_FDCWD, a[0], a[1] as i32),
        24 => 0, // sched_yield：单线程下无事可做
        25 => sys_mremap(m, a[0], a[1], a[2], a[3] as i32, a[4]),
        28 => 0, // madvise：建议性，忽略即为正确实现
        32 => sys_dup(m, a[0] as i32),
        33 => sys_dup2(m, a[0] as i32, a[1] as i32),
        39 | 102 | 104 | 107 | 108 | 110 | 111 => match nr {
            39 => m.os.pid as i64,   // getpid
            110 => m.os.ppid as i64, // getppid
            111 => m.os.pid as i64,  // getpgrp
            _ => 0,                  // getuid/getgid/geteuid/getegid：容器内是 root
        },
        // ---- 进程族（快照式 fork，见 syscall/process.rs） ----
        56 => process::sys_clone(m, a[0], a[1], ret_rip),
        // fork / vfork 都没有参数。vfork 的语义是"父进程挂起到子进程
        // exec 或退出"——快照式 fork 恰好就是它的超集，所以同一份实现。
        57 | 58 => process::sys_fork(m, 0, ret_rip),
        61 => process::sys_wait4(m, a[0] as i32, a[1], a[2] as i32),
        // nanosleep / clock_nanosleep：真的睡。clock_nanosleep 的 req 在 a[2]。
        35 => process::sys_nanosleep(m, a[0], a[1]),
        230 => process::sys_nanosleep(m, a[2], a[3]),
        // pause / sigsuspend：等不到的等待，见 pause_deadlock 的说明。
        34 | 130 => return Err(process::pause_deadlock(m)),
        // kill/tkill/tgkill：给自己发致命信号必须真的终止（abort() 靠它）。
        62 => process::sys_kill(m, a[0] as i32, a[1] as i32)?,
        200 => process::sys_kill(m, a[0] as i32, a[1] as i32)?,
        234 => process::sys_kill(m, a[0] as i32, a[2] as i32)?,
        // 进程组/会话：模拟器里只有一个组、一个会话，`setpgid` 无副作用，
        // 查询一律回自己的 pid（shell 的作业控制会读这几个值做判断）。
        109 => 0,                           // setpgid
        112 | 121 | 124 => m.os.pid as i64, // setsid / getpgid / getsid
        60 | 231 => return Err(Exception::Exit(a[0] as i32 & 0xff)),
        63 => sys_uname(m, a[0]),
        72 => sys_fcntl(m, a[0] as i32, a[1] as i32, a[2]),
        79 => sys_getcwd(m, a[0], a[1]),
        80 => sys_chdir(m, a[0]),
        87 => sys_unlinkat(m, AT_FDCWD, a[0], 0),
        89 => sys_readlinkat(m, AT_FDCWD, a[0], a[1], a[2]),
        96 => sys_gettimeofday(m, a[0], a[1]),
        97 | 160 | 302 => sys_rlimit(m, nr, &a),
        // futex：单线程模拟器里 WAIT 不可能被唤醒，WAKE 没有等待者。
        // 直接成功比 ENOSYS 好——glibc/musl 的锁在无竞争路径上根本不会走到这里。
        202 => 0,
        217 => sys_getdents64(m, a[0] as i32, a[1], a[2]),
        218 => {
            m.os.tid_address = a[0];
            m.os.pid as i64
        }
        228 => sys_clock_gettime(m, a[0] as i32, a[1]),
        257 => sys_openat(m, a[0] as i32, a[1], a[2] as i32, a[3] as u32),
        262 => sys_newfstatat(m, a[0] as i32, a[1], a[2], a[3] as i32),
        263 => sys_unlinkat(m, a[0] as i32, a[1], a[2] as i32),
        267 => sys_readlinkat(m, a[0] as i32, a[1], a[2], a[3]),
        158 => sys_arch_prctl(m, a[0] as i32, a[1]),
        201 => sys_time(m, a[0]),
        // set_robust_list / rseq：线程相关的登记，单线程下无副作用
        273 | 334 => 0,
        318 => sys_getrandom(m, a[0], a[1], a[2] as u32),
        // ---- 文件系统写操作与 fd 杂项 ----
        7 => sys_poll(m, a[0], a[1], a[2] as i32),
        18 => sys_pwrite(m, a[0] as i32, a[1], a[2], a[3] as i64),
        22 => sys_pipe(m, a[0], 0),
        293 => sys_pipe(m, a[0], a[1] as i32),
        26 => sys_msync(m, a[0], a[1], a[2] as i32),
        40 => sys_sendfile(m, a[0] as i32, a[1] as i32, a[2], a[3]),
        74 | 75 => sys_fsync(m, a[0] as i32),
        76 => sys_truncate(m, a[0], a[1] as i64),
        165 => sys_mount(m, a[0], a[1], a[2], a[3], a[4]),
        77 => sys_ftruncate(m, a[0] as i32, a[1] as i64),
        81 => sys_fchdir(m, a[0] as i32),
        82 => sys_rename(m, AT_FDCWD, a[0], AT_FDCWD, a[1]),
        264 | 316 => sys_rename(m, a[0] as i32, a[1], a[2] as i32, a[3]),
        83 => sys_mkdir(m, AT_FDCWD, a[0]),
        258 => sys_mkdir(m, a[0] as i32, a[1]),
        84 => sys_unlinkat(m, AT_FDCWD, a[0], AT_REMOVEDIR),
        85 => sys_openat(m, AT_FDCWD, a[0], O_WRONLY | O_CREAT | O_TRUNC, a[1] as u32),
        86 => sys_linkat(m, AT_FDCWD, a[0], AT_FDCWD, a[1], 0),
        265 => sys_linkat(m, a[0] as i32, a[1], a[2] as i32, a[3], a[4] as i32),
        88 => sys_symlink(m, a[0], a[1]),
        266 => sys_symlink(m, a[0], a[2]),
        // chmod/chown 族：容器内一律 root，且 Windows 没有对应语义。
        // 报成功而不是 ENOSYS——guest 的 install/cp 会因为 chmod 失败而整体失败，
        // 而这里的"失败"并不代表任何真实的权限问题。
        // chmod/fchmod/chown/... 本引擎不落实权限位（容器内一律 root），
        // **但只读挂载下必须报 EROFS**——否则 guest 会以为改成功了。
        90 => sys_readonly_guard(m, a[0]),
        268 => sys_readonly_guard_at(m, a[0] as i32, a[1]),
        91 | 92 | 93 | 260 => 0,
        // utimensat 族：时间戳设置暂不落到宿主，报成功。
        132 | 235 | 280 => 0,
        95 => {
            // umask：只记住值并返回旧值，不参与实际创建权限（同上）
            let old = m.os.umask;
            m.os.umask = a[0] as u32 & 0o777;
            old as i64
        }
        269 => sys_faccessat(m, a[0] as i32, a[1], a[2] as i32),
        // faccessat2 只是多了一个 flags 参数，可用性判断本身不变。
        439 => sys_faccessat(m, a[0] as i32, a[1], a[2] as i32),
        292 => sys_dup3(m, a[0] as i32, a[1] as i32, a[2] as i32),
        // ---- socket 族（AF_UNIX 走引擎内实现，见 syscall::net）----
        // eventfd(init) / eventfd2(init, flags)
        // timerfd_create / timerfd_settime / timerfd_gettime
        // 信号：屏蔽字/挂起集合/signalfd/ITIMER_REAL（不含 handler 投递）
        13 => sys_rt_sigaction(m, a[0] as i32, a[1], a[2]),
        14 => sys_rt_sigprocmask(m, a[0] as i32, a[1], a[2], a[3]),
        127 => sys_rt_sigpending(m, a[0], a[1]),
        282 => sys_signalfd(m, a[0] as i32, a[1], a[2], 0),
        289 => sys_signalfd(m, a[0] as i32, a[1], a[2], a[3] as i32),
        38 => sys_setitimer(m, a[0] as i32, a[1], a[2]),
        283 => sys_timerfd_create(m, a[0] as i32, a[1] as i32),
        286 => sys_timerfd_settime(m, a[0] as i32, a[1] as i32, a[2], a[3]),
        287 => sys_timerfd_gettime(m, a[0] as i32, a[1]),
        284 => sys_eventfd(m, a[0], 0),
        290 => sys_eventfd(m, a[0], a[1] as i32),
        41 => sys_socket(m, a[0] as i32, a[1] as i32, a[2] as i32),
        42 => sys_connect(m, a[0] as i32, a[1], a[2] as u32),
        43 => sys_accept4(m, a[0] as i32, a[1], a[2], 0),
        288 => sys_accept4(m, a[0] as i32, a[1], a[2], a[3] as i32),
        44 => sys_sendto(m, a[0] as i32, a[1], a[2], a[3] as i32, a[4], a[5] as u32),
        45 => sys_recvfrom(m, a[0] as i32, a[1], a[2], a[3] as i32, a[4], a[5]),
        48 => sys_shutdown(m, a[0] as i32, a[1] as i32),
        49 => sys_bind(m, a[0] as i32, a[1], a[2] as u32),
        50 => sys_listen(m, a[0] as i32, a[1] as i32),
        51 => sys_getsockname(m, a[0] as i32, a[1], a[2]),
        52 => sys_getpeername(m, a[0] as i32, a[1], a[2]),
        53 => sys_socketpair(m, a[0] as i32, a[1] as i32, a[2] as i32, a[3]),
        54 => sys_setsockopt(m, a[0] as i32, a[1] as i32, a[2] as i32, a[3], a[4] as u32),
        55 => sys_getsockopt(m, a[0] as i32, a[1] as i32, a[2] as i32, a[3], a[4]),
        // ---- epoll ----
        213 => sys_epoll_create1(m, 0),
        291 => sys_epoll_create1(m, a[0] as i32),
        233 => sys_epoll_ctl(m, a[0] as i32, a[1] as i32, a[2] as i32, a[3]),
        232 | 281 => sys_epoll_wait(m, a[0] as i32, a[1], a[2] as i32, a[3] as i32),
        332 => sys_statx(m, a[0] as i32, a[1], a[2] as i32, a[4]),
        _ => {
            if m.os.strace {
                eprintln!("wbox-linux: (sys) syscall {nr} 未实现 -> ENOSYS");
            }
            -ENOSYS
        }
    };

    m.cpu.regs[RAX] = ret as u64;
    // syscall 指令会破坏 rcx 和 r11（硬件用它们保存 rip/rflags）。
    // 有些手写汇编依赖这一点，所以照实模拟。
    m.cpu.regs[RCX] = ret_rip;
    m.cpu.regs[R11] = m.cpu.flags.pack();
    m.cpu.rip = ret_rip;
    Ok(())
}

/// 读 guest 里的 NUL 结尾路径。
fn guest_path(m: &Machine, ptr: u64) -> Result<String, i64> {
    if ptr == 0 {
        return Err(-EFAULT);
    }
    let bytes = m.mem.read_cstr(ptr, PATH_MAX).map_err(|_| -EFAULT)?;
    if bytes.len() >= PATH_MAX {
        return Err(-ENAMETOOLONG);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// 把 `dirfd` + 路径解析成宿主路径。只支持 `AT_FDCWD` 与已打开的目录 fd。
///
/// `follow`：末段是符号链接时要不要展开。**没有默认值是有意的**——
/// 每个调用点都必须显式回答"这个 syscall 跟随末段吗"。加个薄包装省掉这个
/// 参数，就等于给了"忘记想"的机会，而想错的那一半是静默的越狱。
fn resolve_at(
    m: &Machine,
    dirfd: i32,
    path: &str,
    follow: bool,
) -> Result<std::path::PathBuf, i64> {
    if path.starts_with('/') || dirfd == AT_FDCWD {
        // 越根尝试直接拒绝（不是"夹到根"）；受限解析见 Vfs::translate
        // 失败原因原样转成 errno：越根 -> EACCES，链接成环 -> ELOOP。
        // 合并成一档会让 guest 在成环时看到"权限不足"，与真实内核不符。
        return if follow {
            m.os.vfs.host_path_confined(path)
        } else {
            m.os.vfs.host_path_confined_nofollow(path)
        }
        .map_err(|e| e.errno());
    }
    match m.os.fds.get(dirfd).map(|f| &f.kind) {
        Some(FdKind::Dir { path: dir, .. }) => {
            // **走与绝对路径同一套受限解析**，不再自带一份判据。
            //
            // 早先这里用一个朴素的深度计数器：`..` 减一、普通段加一，减到
            // 负就拒。它有两个毛病，都是"第二份实现"必然带来的：
            //
            // 1. **太严**。从 rootfs 里某个子目录的 dirfd 出发，`openat(dfd,
            //    "..")` 只是回到父目录，完全在 rootfs 内，却被判成越界。
            //    `tests/guest/t_sec_path.c` 的 openat-dirfd-anchor 就断言了
            //    这该成功。
            // 2. **管不到符号链接**。它只看路径字面量，dirfd 底下一个指向
            //    外部的链接照样能顺出去。
            //
            // 现在把 dirfd 的宿主路径还原成 guest 视角路径，与相对路径拼好
            // 之后交给 `Vfs` 那套解析。一份实现，两条路共用。
            let Some(pre) = m.os.vfs.prefix.as_ref() else {
                // 直通模式没有"根"可越，拼上就是。
                return Ok(dir.join(path));
            };
            let guest_dir = match dir.strip_prefix(pre) {
                Ok(rel) => format!("/{}", rel.to_string_lossy()),
                // dirfd 落在 prefix 之外：这本身就不该发生（所有 fd 都由
                // 受限解析产生）。保守拒绝而不是当成根。
                Err(_) => return Err(-EACCES),
            };
            let joined = if guest_dir.ends_with('/') {
                format!("{guest_dir}{path}")
            } else {
                format!("{guest_dir}/{path}")
            };
            if follow {
                m.os.vfs.host_path_confined(&joined)
            } else {
                m.os.vfs.host_path_confined_nofollow(&joined)
            }
            .map_err(|e| e.errno())
        }
        Some(_) => Err(-ENOTDIR),
        None => Err(-EBADF),
    }
}

// ------------------------------------------------------------------ IO

fn sys_read(m: &mut Machine, fd: i32, buf: u64, count: u64) -> i64 {
    let n = count.min(1 << 20) as usize; // 单次上限，避免 guest 传巨值就吃光宿主内存
    let mut tmp = vec![0u8; n];
    let got = match m.os.fds.get_mut(fd).map(|f| &mut f.kind) {
        Some(FdKind::Stdin) => std::io::stdin().read(&mut tmp),
        Some(FdKind::File(f)) => f.read(&mut tmp),
        Some(FdKind::Dir { .. }) => return -EISDIR,
        Some(FdKind::Dev(d)) => match d {
            fs::DevKind::Null => Ok(0), // EOF
            fs::DevKind::Zero | fs::DevKind::Full => {
                tmp.fill(0);
                Ok(n)
            }
            // /dev/{u,}random 走和 getrandom(2) 同一个宿主 CSPRNG。
            fs::DevKind::Random => match host_random(&mut tmp) {
                Ok(()) => Ok(n),
                Err(e) => return e,
            },
            fs::DevKind::Tty => std::io::stdin().read(&mut tmp),
        },
        Some(FdKind::PipeWrite(_)) => return -EBADF, // 写端不可读
        Some(FdKind::Epoll(_)) => return -EINVAL,
        // eventfd：一次必须正好 8 字节，见 `EventFd` 的说明。
        Some(FdKind::Event(e)) => {
            if n < 8 {
                return -EINVAL;
            }
            let Some(v) = e.take() else {
                return -EAGAIN;
            };
            tmp[..8].copy_from_slice(&v.to_le_bytes());
            Ok(8)
        }
        // signalfd：一次至少一条 128 字节记录，能装下几条就返回几条。
        Some(FdKind::Signal(g)) => {
            const REC: usize = 128;
            if n < REC {
                return -EINVAL;
            }
            let mask = g.mask.get();
            let nb = m.os.fds.get(fd).map(|f| f.flags()).unwrap_or(0) & O_NONBLOCK != 0;
            let mut off = 0usize;
            while off + REC <= n {
                let Some((sig, code, pid)) = take_pending(m, mask) else {
                    break;
                };
                write_signalfd_siginfo(&mut tmp[off..off + REC], sig, code, pid);
                off += REC;
            }
            if off == 0 {
                if nb {
                    return -EAGAIN;
                }
                // 阻塞读：唯一可能自己冒出来的信号是 ITIMER_REAL 的 SIGALRM
                // （时间在走）。等它，带上限兜底——别的信号在单线程里没有
                // 来源，等下去就是挂死。
                let dl = m.os.alarm_deadline_ns;
                if dl == 0 {
                    return -EAGAIN;
                }
                let wait = dl.saturating_sub(now_ns()).min(5_000_000_000);
                std::thread::sleep(std::time::Duration::from_nanos(wait));
                let Some((sig, code, pid)) = take_pending(m, mask) else {
                    return -EAGAIN;
                };
                write_signalfd_siginfo(&mut tmp[..REC], sig, code, pid);
                off = REC;
            }
            Ok(off)
        }
        // timerfd 与 eventfd 同形：一次正好 8 字节，读走到期次数并清零。
        Some(FdKind::Timer(t)) => {
            if n < 8 {
                return -EINVAL;
            }
            let t = Rc::clone(t);
            let nb = m.os.fds.get(fd).map(|f| f.flags()).unwrap_or(0) & O_NONBLOCK != 0;
            let v = timer_read(&t, nb);
            if v == 0 {
                return -EAGAIN;
            }
            tmp[..8].copy_from_slice(&v.to_le_bytes());
            Ok(8)
        }
        Some(FdKind::Socket(_)) => {
            // 借用冲突：`fds.get_mut` 的可变借用还在，先把 Rc 取出来。
            let s = match sock_of(m, fd) {
                Ok(s) => s,
                Err(e) => return e,
            };
            if s.is_inet() {
                let nb = m.os.fds.get(fd).map(|f| f.flags()).unwrap_or(0) & O_NONBLOCK != 0;
                if matches!(&*s.inet.borrow(), net::Inet::Udp(_)) {
                    let st = s.inet.borrow();
                    let net::Inet::Udp(u) = &*st else {
                        return -ENOTCONN;
                    };
                    let _ = u.set_nonblocking(nb);
                    match u.recv(&mut tmp) {
                        Ok(k) => Ok(k),
                        Err(e) => return host_err(&e),
                    }
                } else {
                    match net::inet_io(&s, nb) {
                        Ok(mut t) => t.read(&mut tmp),
                        Err(e) => return e,
                    }
                }
            } else {
                match net::recv(&s, &mut tmp, false) {
                    Ok(k) => Ok(k),
                    Err(e) => return e,
                }
            }
        }
        Some(FdKind::PipeRead(r)) => {
            let inner = r.inner();
            let mut q = inner.data.borrow_mut();
            if q.is_empty() {
                if inner.writers_closed() {
                    // 写端全关了：这是 EOF，必须返回 0。快照式 fork 下
                    // `$(cmd)` 的子进程早已退出、写端随它的 fd 表一起析构，
                    // 若这里还报 EAGAIN，父进程会在读端上无限自旋。
                    return 0;
                }
                // 还有写端开着。单线程下没人能在我们阻塞期间写入，阻塞必然
                // 死锁；报 EAGAIN 把决定权交回 guest，而不是挂住整个进程。
                return -EAGAIN;
            }
            let k = n.min(q.len());
            for (i, b) in q.drain(..k).enumerate() {
                tmp[i] = b;
            }
            Ok(k)
        }
        Some(_) => return -EBADF,
        None => return -EBADF,
    };
    match got {
        Ok(k) => {
            if m.mem.write(buf, &tmp[..k]).is_err() {
                return -EFAULT;
            }
            k as i64
        }
        Err(e) => host_err(&e),
    }
}

fn sys_pread(m: &mut Machine, fd: i32, buf: u64, count: u64, off: i64) -> i64 {
    // eventfd 是**可寻址**的（Linux 上 lseek 返回 0、pread/pwrite 照常工作），
    // 只是偏移被忽略。所以不能和管道一起归到 ESPIPE 那一档。
    if matches!(
        m.os.fds.get(fd).map(|f| &f.kind),
        Some(FdKind::Event(_)) | Some(FdKind::Timer(_))
    ) {
        return sys_read(m, fd, buf, count);
    }
    let n = count.min(1 << 20) as usize;
    let mut tmp = vec![0u8; n];
    let got = match m.os.fds.get_mut(fd).map(|f| &mut f.kind) {
        Some(FdKind::File(f)) => {
            // 保存/恢复文件位置：pread 不应该改变它
            let cur = match f.stream_position() {
                Ok(c) => c,
                Err(e) => return host_err(&e),
            };
            let r = f
                .seek(SeekFrom::Start(off as u64))
                .and_then(|_| f.read(&mut tmp));
            let _ = f.seek(SeekFrom::Start(cur));
            r
        }
        // 管道与字符设备不可寻址：Linux 给 ESPIPE，不是 EBADF。
        Some(FdKind::PipeRead(_))
        | Some(FdKind::PipeWrite(_))
        | Some(FdKind::Dev(_))
        | Some(FdKind::Stdin)
        | Some(FdKind::Stdout)
        | Some(FdKind::Stderr) => return -ESPIPE,
        Some(_) => return -EBADF,
        None => return -EBADF,
    };
    match got {
        Ok(k) => {
            if m.mem.write(buf, &tmp[..k]).is_err() {
                return -EFAULT;
            }
            k as i64
        }
        Err(e) => host_err(&e),
    }
}

fn sys_write(m: &mut Machine, fd: i32, buf: u64, count: u64) -> i64 {
    let n = count.min(1 << 20) as usize;
    let mut tmp = vec![0u8; n];
    if m.mem.read(buf, &mut tmp).is_err() {
        return -EFAULT;
    }
    write_bytes(m, fd, &tmp)
}

fn write_bytes(m: &mut Machine, fd: i32, data: &[u8]) -> i64 {
    // 状态标志属于**打开文件描述**，先取出来再借 kind——两者都从同一个 `Fd`
    // 上拿，借用期会打架。
    let status = match m.os.fds.get(fd) {
        Some(f) => f.flags(),
        None => return -EBADF,
    };
    let nonblock = status & O_NONBLOCK != 0;
    let append = status & O_APPEND != 0;
    let r = match m.os.fds.get_mut(fd).map(|f| &mut f.kind) {
        Some(FdKind::Stdout) => {
            let mut o = std::io::stdout();
            o.write_all(data)
                .and_then(|_| o.flush())
                .map(|_| data.len())
        }
        Some(FdKind::Stderr) => {
            let mut o = std::io::stderr();
            o.write_all(data)
                .and_then(|_| o.flush())
                .map(|_| data.len())
        }
        Some(FdKind::File(f)) => {
            // O_APPEND：每次写之前原子地移到文件末尾。它是**描述**上的标志，
            // 所以 `dup` 出来的别名上 `F_SETFL O_APPEND` 之后，这条 fd 的写
            // 也必须变成追加——`t_fd_open` 的 dup/shared-append-status 正是
            // 先 lseek 回 0 再写，然后断言内容被追加在了尾巴上。
            if append {
                if let Err(e) = f.seek(SeekFrom::End(0)) {
                    return host_err(&e);
                }
            }
            f.write(data)
        }
        Some(FdKind::Dir { .. }) => return -EBADF,
        Some(FdKind::Dev(d)) => match d {
            // /dev/full 的存在意义就是"写入必失败"，别给它成功。
            fs::DevKind::Full => return -ENOSPC,
            fs::DevKind::Null | fs::DevKind::Zero | fs::DevKind::Random => Ok(data.len()),
            fs::DevKind::Tty => {
                let mut o = std::io::stdout();
                o.write_all(data)
                    .and_then(|_| o.flush())
                    .map(|_| data.len())
            }
        },
        Some(FdKind::PipeRead(_)) => return -EBADF, // 读端不可写
        Some(FdKind::Epoll(_)) => return -EINVAL,
        // timerfd / signalfd 不可写：Linux 报 EINVAL。
        Some(FdKind::Timer(_)) | Some(FdKind::Signal(_)) => return -EINVAL,
        Some(FdKind::Event(e)) => {
            if data.len() < 8 {
                return -EINVAL;
            }
            let v = u64::from_le_bytes(data[..8].try_into().unwrap());
            // 0xffff_ffff_ffff_ffff 是保留值，Linux 报 EINVAL 而不是溢出。
            if v == u64::MAX {
                return -EINVAL;
            }
            if e.add(v) {
                Ok(8)
            } else {
                return -EAGAIN;
            }
        }
        Some(FdKind::Socket(_)) => {
            let s = match sock_of(m, fd) {
                Ok(s) => s,
                Err(e) => return e,
            };
            if s.is_inet() {
                if matches!(&*s.inet.borrow(), net::Inet::Udp(_)) {
                    // 未 connect 的数据报 socket 上直接 write：没有目标地址，
                    // Linux 报 EDESTADDRREQ。用 sendto 才对。
                    return -EDESTADDRREQ;
                }
                match net::inet_io(&s, nonblock) {
                    Ok(mut t) => t.write(data),
                    Err(e) => return e,
                }
            } else {
                match net::send(&s, data, nonblock) {
                    Ok(k) => Ok(k),
                    Err(e) => return e,
                }
            }
        }
        Some(FdKind::PipeWrite(w)) => {
            let inner = w.inner();
            // 读端全关：真内核会同时发 SIGPIPE 并返回 EPIPE。信号投递本引擎
            // 还没有（C 组），但 errno 这一半是能给对的，先给对。
            if inner.readers_closed() {
                return -EPIPE;
            }
            if nonblock {
                // 非阻塞写：容量是硬边界，写不下就 EAGAIN、写得下多少写多少。
                // 见 `PipeInner::capacity` 的说明——容量**只**在这条路上强制，
                // 阻塞写允许超出，否则单线程下的 `a | b` 会死锁。
                let space = inner.space();
                if space == 0 {
                    return -EAGAIN;
                }
                let k = space.min(data.len());
                inner.data.borrow_mut().extend(data[..k].iter().copied());
                inner.bump_epoch();
                Ok(k)
            } else {
                inner.data.borrow_mut().extend(data.iter().copied());
                inner.bump_epoch();
                Ok(data.len())
            }
        }
        _ => return -EBADF,
    };
    match r {
        Ok(k) => k as i64,
        Err(e) => host_err(&e),
    }
}

/// `iovec { void *base; size_t len; }`
fn read_iovec(m: &Machine, ptr: u64, cnt: u64) -> Result<Vec<(u64, u64)>, i64> {
    if cnt > 1024 {
        return Err(-EINVAL);
    }
    let mut v = Vec::with_capacity(cnt as usize);
    for i in 0..cnt {
        let base = m.mem.read_u64(ptr + i * 16).map_err(|_| -EFAULT)?;
        let len = m.mem.read_u64(ptr + i * 16 + 8).map_err(|_| -EFAULT)?;
        v.push((base, len));
    }
    Ok(v)
}

/// eventfd 的 `readv`：按总长度读一次再散布。非 eventfd 返回 `None`。
fn eventfd_scatter_read(m: &mut Machine, fd: i32, iov: &[(u64, u64)]) -> Option<i64> {
    enum Src {
        Event(Rc<fs::EventFd>),
        Timer(Rc<fs::TimerFd>),
        Signal(Rc<fs::SignalFd>),
    }
    let src = match m.os.fds.get(fd).map(|f| &f.kind) {
        Some(FdKind::Event(e)) => Src::Event(Rc::clone(e)),
        Some(FdKind::Timer(t)) => Src::Timer(Rc::clone(t)),
        Some(FdKind::Signal(g)) => Src::Signal(Rc::clone(g)),
        _ => return None,
    };
    let total: u64 = iov.iter().map(|(_, l)| *l).sum();
    // signalfd 的记录是 128 字节，与 event/timer 的 8 字节不同。
    if let Src::Signal(g) = &src {
        const REC: usize = 128;
        if (total as usize) < REC {
            return Some(-EINVAL);
        }
        let Some((sig, code, pid)) = take_pending(m, g.mask.get()) else {
            return Some(-EAGAIN);
        };
        let mut rec = [0u8; REC];
        write_signalfd_siginfo(&mut rec, sig, code, pid);
        let mut off = 0usize;
        for (base, len) in iov {
            if off >= REC {
                break;
            }
            let k = (*len as usize).min(REC - off);
            if m.mem.write(*base, &rec[off..off + k]).is_err() {
                return Some(-EFAULT);
            }
            off += k;
        }
        return Some(REC as i64);
    }
    if total < 8 {
        return Some(-EINVAL);
    }
    let nb = m.os.fds.get(fd).map(|f| f.flags()).unwrap_or(0) & O_NONBLOCK != 0;
    let got: Option<u64> = match &src {
        Src::Event(e) => e.take(),
        Src::Timer(t) => match timer_read(t, nb) {
            0 => None,
            v => Some(v),
        },
        // signalfd 已在上面按 128 字节记录处理并提前返回。
        Src::Signal(_) => unreachable!("signalfd 走的是上面的分支"),
    };
    let Some(v) = got else {
        return Some(-EAGAIN);
    };
    let bytes = v.to_le_bytes();
    let mut off = 0usize;
    for (base, len) in iov {
        if off >= 8 {
            break;
        }
        let k = (*len as usize).min(8 - off);
        if m.mem.write(*base, &bytes[off..off + k]).is_err() {
            return Some(-EFAULT);
        }
        off += k;
    }
    Some(8)
}

fn sys_writev(m: &mut Machine, fd: i32, ptr: u64, cnt: u64) -> i64 {
    if !m.os.fds.contains(fd) {
        return -EBADF;
    }
    let iov = match read_iovec(m, ptr, cnt) {
        Ok(v) => v,
        Err(e) => return e,
    };
    // 先全部拼起来再一次写出：writev 语义上是原子的，分多次 write 会让
    // 交错输出（尤其 stderr）与真实 Linux 不一致。
    let mut all = Vec::new();
    for (base, len) in iov {
        let mut tmp = vec![0u8; len.min(1 << 20) as usize];
        if m.mem.read(base, &mut tmp).is_err() {
            return -EFAULT;
        }
        all.extend_from_slice(&tmp);
    }
    write_bytes(m, fd, &all)
}

fn sys_readv(m: &mut Machine, fd: i32, ptr: u64, cnt: u64) -> i64 {
    // fd 要先校验：`readv(坏fd, NULL, 0)` 必须 EBADF。少了这一句，
    // iov 为空时循环一次都不跑，坏 fd 会被报成"成功读了 0 字节"。
    if !m.os.fds.contains(fd) {
        return -EBADF;
    }
    let iov = match read_iovec(m, ptr, cnt) {
        Ok(v) => v,
        Err(e) => return e,
    };
    // eventfd 没有缓冲区，一次操作就是**整整 8 字节**——逐个 iovec 调用会
    // 让 4+4 这种拆法每段都短于 8 而报 EINVAL。所以先按总长度读一次，
    // 再散布到各段。writev 同理。
    if let Some(r) = eventfd_scatter_read(m, fd, &iov) {
        return r;
    }
    let mut total = 0i64;
    for (base, len) in iov {
        if len == 0 {
            continue;
        }
        let r = sys_read(m, fd, base, len);
        if r < 0 {
            return if total > 0 { total } else { r };
        }
        total += r;
        if (r as u64) < len {
            break; // 短读，停止
        }
    }
    total
}

/// `preadv` / `pwritev`：逐个 iovec 走 `pread`/`pwrite`，偏移自增。
///
/// 不可寻址的 fd（管道、字符设备）由 `sys_pread`/`sys_pwrite` 报 `ESPIPE`；
/// 这里同样要先校验 fd，理由和 `sys_readv` 一样。
fn sys_preadv(m: &mut Machine, fd: i32, ptr: u64, cnt: u64, off: i64) -> i64 {
    if !m.os.fds.contains(fd) {
        return -EBADF;
    }
    if off < 0 {
        return -EINVAL;
    }
    let iov = match read_iovec(m, ptr, cnt) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut total = 0i64;
    let mut at = off;
    for (base, len) in iov {
        if len == 0 {
            continue;
        }
        let r = sys_pread(m, fd, base, len, at);
        if r < 0 {
            return if total > 0 { total } else { r };
        }
        total += r;
        at += r;
        if (r as u64) < len {
            break; // 短读
        }
    }
    total
}

fn sys_pwritev(m: &mut Machine, fd: i32, ptr: u64, cnt: u64, off: i64) -> i64 {
    if !m.os.fds.contains(fd) {
        return -EBADF;
    }
    if off < 0 {
        return -EINVAL;
    }
    let iov = match read_iovec(m, ptr, cnt) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut total = 0i64;
    let mut at = off;
    for (base, len) in iov {
        if len == 0 {
            continue;
        }
        let r = sys_pwrite(m, fd, base, len, at);
        if r < 0 {
            return if total > 0 { total } else { r };
        }
        total += r;
        at += r;
        if (r as u64) < len {
            break;
        }
    }
    total
}

fn sys_close(m: &mut Machine, fd: i32) -> i64 {
    match m.os.fds.remove(fd) {
        Some(_) => 0,
        None => -EBADF,
    }
}

fn sys_lseek(m: &mut Machine, fd: i32, off: i64, whence: i32) -> i64 {
    let pos = match whence {
        0 => SeekFrom::Start(off as u64),
        1 => SeekFrom::Current(off),
        2 => SeekFrom::End(off),
        _ => return -EINVAL,
    };
    match m.os.fds.get_mut(fd).map(|f| &mut f.kind) {
        Some(FdKind::File(f)) => match f.seek(pos) {
            Ok(p) => p as i64,
            Err(e) => host_err(&e),
        },
        // 字符设备可以 seek，但位置恒为 0（Linux 对 /dev/null 就是这样）。
        Some(FdKind::Dev(_)) => 0,
        // eventfd 可以 seek，但位置恒为 0（与 /dev/null 同理）。
        Some(FdKind::Event(_)) | Some(FdKind::Timer(_)) => 0,
        Some(FdKind::Signal(_)) => -ESPIPE,
        // socket 与管道同档：不可 seek，报 ESPIPE 而不是 EBADF——
        // 后者会让 guest 以为 fd 本身有问题，与真实原因差得很远。
        Some(FdKind::Socket(_)) => -ESPIPE,
        Some(FdKind::Epoll(_)) => -ESPIPE,
        // 标准流可能是管道；管道本身也不可 seek。
        Some(FdKind::Stdin)
        | Some(FdKind::Stdout)
        | Some(FdKind::Stderr)
        | Some(FdKind::PipeRead(_))
        | Some(FdKind::PipeWrite(_)) => -ESPIPE,
        Some(_) => -EBADF,
        None => -EBADF,
    }
}

fn sys_dup(m: &mut Machine, fd: i32) -> i64 {
    dup_impl(m, fd, None)
}

fn sys_dup2(m: &mut Machine, old: i32, new: i32) -> i64 {
    if !m.os.fds.contains(old) {
        return -EBADF;
    }
    if old == new {
        return new as i64;
    }
    dup_impl(m, old, Some(new))
}

/// `dup` 的实现受限于「宿主 fd 无法平台无关地复制」。
/// 文件走 `try_clone`（std 有跨平台实现），标准流按种类复制。
fn dup_impl(m: &mut Machine, fd: i32, at: Option<i32>) -> i64 {
    dup_impl_min(m, fd, at, 3)
}

/// `dup` 系列的实现。`min` 是新 fd 的下界（`F_DUPFD` 要求"不小于 arg 的最小空号"）。
fn dup_impl_min(m: &mut Machine, fd: i32, at: Option<i32>, min: i32) -> i64 {
    let kind = match m.os.fds.get(fd).map(|f| &f.kind) {
        Some(FdKind::Stdin) => FdKind::Stdin,
        Some(FdKind::Stdout) => FdKind::Stdout,
        Some(FdKind::Stderr) => FdKind::Stderr,
        Some(FdKind::File(f)) => match f.try_clone() {
            Ok(c) => FdKind::File(c),
            Err(e) => return host_err(&e),
        },
        Some(FdKind::Dir { path, .. }) => FdKind::Dir {
            path: path.clone(),
            entries: Vec::new(),
            pos: 0,
        },
        // 管道也必须能 dup：shell 做重定向就是 `dup2(pipe_fd, 1)`，
        // 这里漏掉会让 `$(cmd)`、`a | b` 直接拿到 EBADF。
        Some(FdKind::Dev(d)) => FdKind::Dev(*d),
        Some(FdKind::PipeRead(r)) => FdKind::PipeRead(r.clone()),
        Some(FdKind::PipeWrite(w)) => FdKind::PipeWrite(w.clone()),
        Some(FdKind::Socket(s)) => FdKind::Socket(Rc::clone(s)),
        Some(FdKind::Event(e)) => FdKind::Event(Rc::clone(e)),
        Some(FdKind::Timer(t)) => FdKind::Timer(Rc::clone(t)),
        Some(FdKind::Signal(g)) => FdKind::Signal(Rc::clone(g)),
        Some(FdKind::Epoll(e)) => FdKind::Epoll(Rc::clone(e)),
        _ => return -EBADF,
    };
    // dup/dup2 产生的新 fd **不继承** O_CLOEXEC（POSIX 明确规定），但**共享
    // 同一个打开文件描述**——偏移与状态标志（O_APPEND/O_NONBLOCK）都是描述
    // 上的属性，不是描述符上的。`alias` 就是这个区分的落点。
    let Some(nf) = m.os.fds.get(fd).map(|f| f.alias(kind, false)) else {
        return -EBADF;
    };
    match at {
        None => match m.os.fds.alloc_min(nf, min) {
            Some(n) => n as i64,
            None => -EMFILE,
        },
        Some(n) => {
            if n < 0 {
                return -EBADF;
            }
            m.os.fds.insert_at(n, nf);
            n as i64
        }
    }
}

fn sys_fcntl(m: &mut Machine, fd: i32, cmd: i32, arg: u64) -> i64 {
    const F_DUPFD: i32 = 0;
    const F_GETFD: i32 = 1;
    const F_SETFD: i32 = 2;
    const F_GETFL: i32 = 3;
    const F_SETFL: i32 = 4;
    const F_GETLK: i32 = 5;
    const F_SETLK: i32 = 6;
    const F_SETLKW: i32 = 7;
    const F_DUPFD_CLOEXEC: i32 = 1030;
    const F_SETPIPE_SZ: i32 = 1031;
    const F_GETPIPE_SZ: i32 = 1032;
    if !m.os.fds.contains(fd) {
        return -EBADF;
    }
    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            // arg 是新 fd 的**下界**，不是要忽略的东西。负值或超过
            // RLIMIT_NOFILE 上限都必须 EINVAL——否则 guest 拿到一个
            // 它明确要求"不小于 N"却更小的 fd，后续逻辑会以很难查的方式错。
            let min = arg as i64;
            if min < 0 || min > MAX_FD as i64 {
                return -EINVAL;
            }
            let r = dup_impl_min(m, fd, None, min as i32);
            if r >= 0 && cmd == F_DUPFD_CLOEXEC {
                if let Some(f) = m.os.fds.get_mut(r as i32) {
                    f.cloexec = true;
                }
            }
            r
        }
        F_GETFD => m.os.fds.get(fd).map(|f| f.cloexec as i64).unwrap_or(-EBADF),
        F_SETFD => {
            if let Some(f) = m.os.fds.get_mut(fd) {
                f.cloexec = arg & 1 != 0;
            }
            0
        }
        F_GETFL => m.os.fds.get(fd).map(|f| f.flags() as i64).unwrap_or(-EBADF),
        F_SETFL => {
            let Some(f) = m.os.fds.get(fd) else {
                return -EBADF;
            };
            // 只有这几位是 SETFL 可改的
            let keep = f.flags() & !(O_APPEND | O_NONBLOCK);
            f.set_flags(keep | (arg as i32 & (O_APPEND | O_NONBLOCK)));
            0
        }
        // 管道容量。**只对管道有意义**：对别的 fd Linux 返回 EBADF（不是
        // EINVAL），`t_fd_rw` 的 pipe/capacity-fcntl 专门断言了这一点。
        F_GETPIPE_SZ => match m.os.fds.get(fd).map(|f| &f.kind) {
            Some(FdKind::PipeRead(r)) => r.inner().capacity() as i64,
            Some(FdKind::PipeWrite(w)) => w.inner().capacity() as i64,
            Some(_) => -EBADF,
            None => -EBADF,
        },
        F_SETPIPE_SZ => {
            // 0 不是"用默认值"，是非法请求。
            if arg == 0 {
                return -EINVAL;
            }
            match m.os.fds.get(fd).map(|f| &f.kind) {
                // 内核会把小于一页的请求抬到一页，并返回实际采用的值。
                Some(FdKind::PipeRead(r)) => r.inner().set_capacity(arg as usize) as i64,
                Some(FdKind::PipeWrite(w)) => w.inner().set_capacity(arg as usize) as i64,
                Some(_) => -EBADF,
                None => -EBADF,
            }
        }
        // 咨询锁（advisory lock）。模拟器里只有一个 guest 进程在参与锁协议，
        // 所以"能不能拿到锁"的答案恒为能——这不是假装成功，是真的没有竞争者。
        // F_GETLK 据此回 F_UNLCK（l_type 是 struct flock 的第 0 个 short）。
        F_GETLK => {
            const F_UNLCK: u16 = 2;
            if m.mem.write_u16(arg, F_UNLCK).is_err() {
                return -EFAULT;
            }
            0
        }
        F_SETLK | F_SETLKW => {
            // 只校验 arg 可读，别默默吃掉坏指针。
            if m.mem.read_u16(arg).is_err() {
                return -EFAULT;
            }
            0
        }
        _ => -EINVAL,
    }
}

/// `ioctl`。只认 guest 用来判断"是不是 tty"的那几个请求。
fn sys_ioctl(m: &mut Machine, fd: i32, req: u64, _arg: u64) -> i64 {
    const TCGETS: u64 = 0x5401;
    const TIOCGWINSZ: u64 = 0x5413;
    const FIONCLEX: u64 = 0x5450;
    const FIOCLEX: u64 = 0x5451;
    const FIONBIO: u64 = 0x5421;
    const FIONREAD: u64 = 0x541B;
    if !m.os.fds.contains(fd) {
        return -EBADF;
    }
    match req {
        // 一律报"不是终端"。这会让 guest 侧的 libc 选行缓冲/全缓冲里的
        // 全缓冲、让 ls 输出单列——语义正确且可预期。真正的 pty 支持
        // 需要宿主侧的伪终端，属于后续里程碑。
        TCGETS | TIOCGWINSZ => -ENOTTY,
        // FIOCLEX / FIONCLEX 与 tty 无关：它们等价于 fcntl(F_SETFD)，
        // 对任何 fd 都该生效。报 ENOTTY 是错的。
        FIOCLEX | FIONCLEX => {
            if let Some(f) = m.os.fds.get_mut(fd) {
                f.cloexec = req == FIOCLEX;
            }
            0
        }
        // FIONREAD：可立即读出的字节数。管道与 socket 都要答得上来——
        // 很多程序拿它决定缓冲区大小，报 ENOTTY 会让它们退化成逐字节读。
        FIONREAD => {
            let n = match m.os.fds.get(fd).map(|f| &f.kind) {
                Some(FdKind::Socket(s)) => net::readable_bytes(s),
                Some(FdKind::PipeRead(r)) => r.inner().data.borrow().len(),
                Some(FdKind::PipeWrite(_)) => 0,
                Some(_) => return -ENOTTY,
                None => return -EBADF,
            };
            if m.mem.write_u32(_arg, n as u32).is_err() {
                return -EFAULT;
            }
            0
        }
        // FIONBIO 是 `F_SETFL O_NONBLOCK` 的另一种写法，作用在**打开文件
        // 描述**上，因此对所有别名同时生效。同样与 tty 无关，报 ENOTTY 是错的。
        FIONBIO => {
            let on = match m.mem.read_u32(_arg) {
                Ok(v) => v != 0,
                Err(_) => return -EFAULT,
            };
            let Some(f) = m.os.fds.get(fd) else {
                return -EBADF;
            };
            let cur = f.flags();
            f.set_flags(if on {
                cur | O_NONBLOCK
            } else {
                cur & !O_NONBLOCK
            });
            0
        }
        _ => -ENOTTY,
    }
}

// ------------------------------------------------------------- open/stat

/// `open(dir, O_TMPFILE|O_RDWR)`：在 `dir` 里开一个**没有名字**的文件。
///
/// 两个平台都能做到"无名"，但机制不同：
///   - Unix：建好立刻 `unlink`。已打开的句柄仍然有效，这正是 POSIX 的语义，
///     也是 glibc 在没有 `O_TMPFILE` 的老内核上的回退做法。
///   - Windows：`FILE_FLAG_DELETE_ON_CLOSE`，并且必须放开 `FILE_SHARE_DELETE`，
///     否则句柄还开着时目录里那个名字删不掉。
///
/// 注意**不能**靠"关 fd 时再删"这类自己记账的方案：`dup` 之后有多个 fd 指向
/// 同一个文件，记账一定会漏，漏了就在 rootfs 里留垃圾——`t_stress` 的
/// 5000 次循环就是专门查这个的。
fn open_tmpfile(m: &mut Machine, dir: &std::path::Path, flags: i32, mode: u32) -> i64 {
    if !dir.is_dir() {
        return -ENOTDIR;
    }
    // O_TMPFILE 必须带写权限（Linux 也这么要求）。
    if flags & O_ACCMODE == O_RDONLY {
        return -EINVAL;
    }
    // 名字只在"建好到 unlink"之间存在，够唯一即可：pid + 单调计数器。
    m.os.tmpfile_seq = m.os.tmpfile_seq.wrapping_add(1);
    let name = format!(".wbox-tmpfile-{}-{}", m.os.pid, m.os.tmpfile_seq);
    let path = dir.join(name);

    let mut opt = std::fs::OpenOptions::new();
    opt.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // 先以 000 创建，再在句柄上设置 guest mode。这样宿主进程自己的
        // umask 不会二次收紧权限，也不存在先暴露过宽权限再修正的窗口。
        opt.mode(0o0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        const FILE_SHARE_WRITE: u32 = 2;
        const FILE_SHARE_DELETE: u32 = 4;
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        opt.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE);
    }
    let f = match opt.open(&path) {
        Ok(f) => f,
        Err(e) => return host_err(&e),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(mode & !m.os.umask & 0o777);
        if let Err(e) = f.set_permissions(permissions) {
            drop(f);
            let _ = std::fs::remove_file(&path);
            return host_err(&e);
        }
    }
    #[cfg(windows)]
    {
        let r = remember_created_mode(m, &f, mode);
        if r != 0 {
            drop(f);
            let _ = std::fs::remove_file(&path);
            return r;
        }
    }
    #[cfg(unix)]
    if let Err(e) = std::fs::remove_file(&path) {
        // 删不掉就别把这个 fd 交出去：交出去 = 在 rootfs 里留下可见垃圾，
        // 而 guest 以为它拿到的是无名文件。
        drop(f);
        let _ = std::fs::remove_file(&path);
        return host_err(&e);
    }
    // O_TMPFILE 之外的 O_DIRECTORY 位不该回给 F_GETFL，摘掉。
    match m.os.fds.alloc(Fd::new(
        FdKind::File(f),
        flags & O_CLOEXEC != 0,
        flags & !(O_TMPFILE | O_DIRECTORY),
    )) {
        Some(n) => n as i64,
        None => -EMFILE,
    }
}

fn sys_openat(m: &mut Machine, dirfd: i32, path_ptr: u64, flags: i32, mode: u32) -> i64 {
    let path = match guest_path(m, path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // open 默认跟随末段符号链接（未实现 O_NOFOLLOW，见 crate 文档）。
    let host = match resolve_at(m, dirfd, &path, true) {
        Ok(p) => p,
        Err(e) => return e,
    };

    // 只读挂载：带写意图（含 O_CREAT/O_TRUNC）的打开一律 EROFS。
    // 这一条要在碰宿主文件系统**之前**判——落到宿主上再失败，errno 会变成
    // EACCES/EPERM 之类，与真实内核对不上。
    if (flags & O_ACCMODE != O_RDONLY || flags & (O_CREAT | O_TRUNC) != 0)
        && m.os.vfs.is_readonly(&path)
    {
        return -EROFS;
    }

    // 空路径永远是 ENOENT。不显式判的话它会被当成"当前目录"，
    // `open("", O_RDONLY)` 就会成功返回一个目录 fd。
    if path.is_empty() {
        return -ENOENT;
    }

    // 合成的 /dev/*：必须在碰宿主文件系统之前拦下来，容器 rootfs 里通常
    // 没有 /dev，Windows 宿主上更是根本没有这些路径。
    if let Some(d) = fs::DevKind::from_guest_path(&path) {
        return match m.os.fds.alloc(Fd::new(
            FdKind::Dev(d),
            flags & O_CLOEXEC != 0,
            flags & !O_CLOEXEC,
        )) {
            Some(n) => n as i64,
            None => -EMFILE,
        };
    }

    // O_TMPFILE 要在"目录"分支之前判：它的路径**就是**一个目录，但要的
    // 不是目录 fd 而是该目录下一个无名的可读写文件。
    if flags & O_TMPFILE != 0 {
        return open_tmpfile(m, &host, flags, mode);
    }

    // 目录：单独一种 fd（getdents64 要用），不能按普通文件打开。
    let is_dir = host.is_dir();
    if is_dir {
        if flags & O_ACCMODE != O_RDONLY {
            return -EISDIR;
        }
        let entries = match read_dir_entries(&host) {
            Ok(e) => e,
            Err(e) => return host_err(&e),
        };
        let fd = Fd::new(
            FdKind::Dir {
                path: host,
                entries,
                pos: 0,
            },
            flags & O_CLOEXEC != 0,
            flags,
        );
        return match m.os.fds.alloc(fd) {
            Some(n) => n as i64,
            None => -EMFILE,
        };
    }
    if flags & O_DIRECTORY != 0 {
        return -ENOTDIR;
    }

    // O_CREAT 只在真正创建 inode 时应用 mode；打开既有文件不能改权限。
    // O_EXCL 成功就必然是新文件，其余情况在碰宿主前记录是否存在。
    let creates_new = if flags & O_CREAT == 0 {
        false
    } else if flags & O_EXCL != 0 {
        true
    } else {
        match host.try_exists() {
            Ok(exists) => !exists,
            Err(e) => return host_err(&e),
        }
    };
    let mut opt = std::fs::OpenOptions::new();
    match flags & O_ACCMODE {
        O_RDONLY => opt.read(true),
        O_WRONLY => opt.write(true),
        O_RDWR => opt.read(true).write(true),
        _ => return -EINVAL,
    };
    if flags & O_APPEND != 0 {
        opt.append(true);
        // append 与 write 在 std 里互斥表达，append 已含写权限
        if flags & O_ACCMODE == O_WRONLY {
            opt.write(false);
        }
    }
    if flags & O_TRUNC != 0 {
        opt.truncate(true);
    }
    if flags & O_CREAT != 0 {
        if flags & O_EXCL != 0 {
            opt.create_new(true);
        } else {
            opt.create(true);
        }
        // O_CREAT 的 mode 必须真的生效：guest 用 0604 之类的位创建文件后会
        // fstat 回来检查。忽略它会让 mode 断言以"权限位不对"的形式失败。
        // Windows 的权限位由 guest 自己按文件 identity 维护，见
        // `remember_created_mode`；不要把宿主固定合成的 0644 暴露给 guest。
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // 宿主 umask 不能参与 guest 语义；成功打开后在句柄上设置最终值。
            opt.mode(0o0);
        }
        #[cfg(not(unix))]
        {
            let _ = mode;
        }
    }
    match opt.open(&host) {
        Ok(f) => {
            #[cfg(unix)]
            if creates_new {
                use std::os::unix::fs::PermissionsExt;
                let permissions = std::fs::Permissions::from_mode(mode & !m.os.umask & 0o777);
                if let Err(e) = f.set_permissions(permissions) {
                    drop(f);
                    let _ = std::fs::remove_file(&host);
                    return host_err(&e);
                }
            }
            #[cfg(windows)]
            if creates_new {
                let r = remember_created_mode(m, &f, mode);
                if r != 0 {
                    drop(f);
                    let _ = std::fs::remove_file(&host);
                    return r;
                }
            }
            let fd = Fd::new(FdKind::File(f), flags & O_CLOEXEC != 0, flags);
            match m.os.fds.alloc(fd) {
                Some(n) => n as i64,
                None => -EMFILE,
            }
        }
        Err(e) => host_err(&e),
    }
}

/// 目录项类型常量（`struct linux_dirent64.d_type`）。
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;

fn read_dir_entries(path: &std::path::Path) -> std::io::Result<Vec<(Vec<u8>, u8)>> {
    // `.` 和 `..` 宿主的 read_dir 不返回，但 guest 期望看到（shell 的 glob、
    // find 的目录遍历都依赖它们存在）。
    let mut out = vec![(b".".to_vec(), DT_DIR), (b"..".to_vec(), DT_DIR)];
    for e in std::fs::read_dir(path)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned().into_bytes();
        let t = match e.file_type() {
            Ok(t) if t.is_dir() => DT_DIR,
            Ok(t) if t.is_symlink() => DT_LNK,
            _ => DT_REG,
        };
        out.push((name, t));
    }
    Ok(out)
}

/// `struct linux_dirent64` 的大小（144 字节的 stat 之外，这个是变长的）：
/// `u64 d_ino; i64 d_off; u16 d_reclen; u8 d_type; char d_name[];`
fn sys_getdents64(m: &mut Machine, fd: i32, buf: u64, count: u64) -> i64 {
    let (entries, pos) = match m.os.fds.get(fd).map(|f| &f.kind) {
        Some(FdKind::Dir { entries, pos, .. }) => (entries.clone(), *pos),
        Some(_) => return -ENOTDIR,
        None => return -EBADF,
    };
    let mut written = 0u64;
    let mut i = pos;
    let mut out: Vec<u8> = Vec::new();
    while i < entries.len() {
        let (name, dtype) = &entries[i];
        // 19 字节头 + 名字 + NUL，按 8 字节对齐
        let reclen = ((19 + name.len() + 1) + 7) & !7;
        if written + reclen as u64 > count {
            break;
        }
        let mut rec = vec![0u8; reclen];
        rec[0..8].copy_from_slice(&((i as u64) + 1).to_le_bytes()); // d_ino（合成）
        rec[8..16].copy_from_slice(&((i as i64) + 1).to_le_bytes()); // d_off
        rec[16..18].copy_from_slice(&(reclen as u16).to_le_bytes());
        rec[18] = *dtype;
        rec[19..19 + name.len()].copy_from_slice(name);
        out.extend_from_slice(&rec);
        written += reclen as u64;
        i += 1;
    }
    if written == 0 && i < entries.len() {
        return -EINVAL; // 缓冲区连一项都放不下
    }
    if m.mem.write(buf, &out).is_err() {
        return -EFAULT;
    }
    if let Some(FdKind::Dir { pos, .. }) = m.os.fds.get_mut(fd).map(|f| &mut f.kind) {
        *pos = i;
    }
    written as i64
}

/// 按 x86-64 的 `struct stat`（144 字节）布局填缓冲区。
fn write_stat_with_identity(
    m: &mut Machine,
    out: u64,
    md: &std::fs::Metadata,
    identity: Option<(u64, u64, u64)>,
) -> i64 {
    let mut b = [0u8; 144];

    // 平台无关部分
    let size = md.len();
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| (d.as_secs(), d.subsec_nanos() as u64))
        .unwrap_or((0, 0));

    // Unix 宿主上用真实值，guest 的 hardlink 计数、inode 比较才有意义。
    #[cfg(unix)]
    let (dev, ino, nlink, uid, gid, blksize, blocks) = {
        use std::os::unix::fs::MetadataExt;
        (
            md.dev(),
            md.ino(),
            md.nlink(),
            md.uid(),
            md.gid(),
            md.blksize() as i64,
            md.blocks() as i64,
        )
    };
    // Windows 宿主：没有 inode，用合成值。guest 若靠 (dev, ino) 判断
    // "是不是同一个文件"会失效，这是已知缺口。
    #[cfg(not(unix))]
    let (dev, ino, nlink, uid, gid, blksize, blocks) = (
        identity.map_or(1, |v| v.0),
        identity.map_or(1, |v| v.1),
        identity.map_or(1, |v| v.2),
        0u32,
        0u32,
        4096i64,
        size.div_ceil(512) as i64,
    );
    let mode = metadata_mode(m, md, identity);

    b[0..8].copy_from_slice(&dev.to_le_bytes());
    b[8..16].copy_from_slice(&ino.to_le_bytes());
    b[16..24].copy_from_slice(&nlink.to_le_bytes());
    b[24..28].copy_from_slice(&mode.to_le_bytes());
    b[28..32].copy_from_slice(&uid.to_le_bytes());
    b[32..36].copy_from_slice(&gid.to_le_bytes());
    // 36..40 是 __pad0
    b[48..56].copy_from_slice(&(size as i64).to_le_bytes());
    b[56..64].copy_from_slice(&blksize.to_le_bytes());
    b[64..72].copy_from_slice(&blocks.to_le_bytes());
    for off in [72usize, 88, 104] {
        b[off..off + 8].copy_from_slice(&mtime.0.to_le_bytes());
        b[off + 8..off + 16].copy_from_slice(&mtime.1.to_le_bytes());
    }

    if m.mem.write(out, &b).is_err() {
        return -EFAULT;
    }
    0
}

fn metadata_mode(m: &Machine, md: &std::fs::Metadata, identity: Option<(u64, u64, u64)>) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = (m, identity);
        md.mode()
    }
    #[cfg(windows)]
    {
        let file_type = if md.is_dir() {
            0o040000
        } else if md.file_type().is_symlink() {
            0o120000
        } else {
            0o100000
        };
        let fallback = if md.is_dir() {
            0o755
        } else if md.file_type().is_symlink() {
            0o777
        } else {
            0o644
        };
        let permissions = identity
            .and_then(|(dev, ino, _)| m.os.file_modes.borrow().get(&(dev, ino)).copied())
            .unwrap_or(fallback);
        file_type | permissions
    }
}

#[cfg(windows)]
fn windows_file_identity(file: &std::fs::File) -> std::io::Result<(u64, u64, u64)> {
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: the raw handle remains owned by `file` for the duration of the
    // call, and Windows initializes the complete output structure on success.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a successful GetFileInformationByHandle call initialized `info`.
    let info = unsafe { info.assume_init() };
    let ino = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
    Ok((
        info.dwVolumeSerialNumber as u64,
        ino,
        info.nNumberOfLinks as u64,
    ))
}

#[cfg(windows)]
fn windows_path_identity(path: &std::path::Path) -> std::io::Result<(u64, u64, u64)> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    const FILE_SHARE_DELETE: u32 = 4;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    windows_file_identity(&file)
}

#[cfg(windows)]
fn remember_created_mode(m: &mut Machine, file: &std::fs::File, requested: u32) -> i64 {
    let (dev, ino, _) = match windows_file_identity(file) {
        Ok(identity) => identity,
        Err(e) => return host_err(&e),
    };
    let permissions = requested & !m.os.umask & 0o777;
    m.os.file_modes.borrow_mut().insert((dev, ino), permissions);
    0
}

fn sys_fstat(m: &mut Machine, fd: i32, out: u64) -> i64 {
    let (md, identity) = match m.os.fds.get(fd).map(|f| &f.kind) {
        Some(FdKind::File(f)) => {
            #[cfg(windows)]
            let identity = match windows_file_identity(f) {
                Ok(identity) => Some(identity),
                Err(e) => return host_err(&e),
            };
            #[cfg(not(windows))]
            let identity = None;
            (f.metadata(), identity)
        }
        Some(FdKind::Dir { path, .. }) => (std::fs::metadata(path), None),
        // socket / 管道 / epoll：合成对应类型的 stat。`S_ISSOCK` 这类判断
        // 在 libc 里很常见，回 EBADF 会让调用方以为 fd 坏了。
        Some(FdKind::Socket(_))
        | Some(FdKind::PipeRead(_))
        | Some(FdKind::PipeWrite(_))
        | Some(FdKind::Epoll(_)) => {
            const S_IFSOCK: u32 = 0o140000;
            const S_IFIFO: u32 = 0o010000;
            let is_sock = matches!(
                m.os.fds.get(fd).map(|f| &f.kind),
                Some(FdKind::Socket(_)) | Some(FdKind::Epoll(_))
            );
            let mut b = [0u8; 144];
            let mode = if is_sock { S_IFSOCK } else { S_IFIFO } | 0o600;
            b[24..28].copy_from_slice(&mode.to_le_bytes());
            b[16..24].copy_from_slice(&1u64.to_le_bytes());
            b[56..64].copy_from_slice(&4096i64.to_le_bytes());
            return if m.mem.write(out, &b).is_err() {
                -EFAULT
            } else {
                0
            };
        }
        // 标准流：合成一个字符设备的 stat。guest 的 isatty/缓冲判断会读它。
        Some(FdKind::Stdin) | Some(FdKind::Stdout) | Some(FdKind::Stderr)
        | Some(FdKind::Dev(_)) => {
            let mut b = [0u8; 144];
            b[24..28].copy_from_slice(&0o020620u32.to_le_bytes()); // S_IFCHR
            b[16..24].copy_from_slice(&1u64.to_le_bytes());
            b[56..64].copy_from_slice(&1024i64.to_le_bytes());
            return if m.mem.write(out, &b).is_err() {
                -EFAULT
            } else {
                0
            };
        }
        _ => return -EBADF,
    };
    match md {
        Ok(md) => write_stat_with_identity(m, out, &md, identity),
        Err(e) => host_err(&e),
    }
}

fn sys_stat_path(m: &mut Machine, dirfd: i32, path_ptr: u64, out: u64, follow: bool) -> i64 {
    let path = match guest_path(m, path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // 合成的 /dev/*：宿主上没有对应的 inode，得自己造一份字符设备 stat。
    // 少了这一条，shell 的 `test -c /dev/null`、`[ -e /dev/null ]` 会说不存在。
    if fs::DevKind::from_guest_path(&path).is_some() {
        let mut b = [0u8; 144];
        b[24..28].copy_from_slice(&0o020666u32.to_le_bytes()); // S_IFCHR | 0666
        b[16..24].copy_from_slice(&1u64.to_le_bytes()); // st_nlink
        b[56..64].copy_from_slice(&4096i64.to_le_bytes()); // st_blksize
        return if m.mem.write(out, &b).is_err() {
            -EFAULT
        } else {
            0
        };
    }
    // stat 跟随、lstat 不跟随——本函数的 follow 参数就是这个区别。
    let host = match resolve_at(m, dirfd, &path, follow) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let md = if follow {
        std::fs::metadata(&host)
    } else {
        std::fs::symlink_metadata(&host)
    };
    match md {
        Ok(md) => {
            #[cfg(windows)]
            let identity = if md.file_type().is_symlink() {
                None
            } else {
                match windows_path_identity(&host) {
                    Ok(identity) => Some(identity),
                    Err(e) => return host_err(&e),
                }
            };
            #[cfg(not(windows))]
            let identity = None;
            write_stat_with_identity(m, out, &md, identity)
        }
        Err(e) => host_err(&e),
    }
}

fn sys_newfstatat(m: &mut Machine, dirfd: i32, path_ptr: u64, out: u64, flags: i32) -> i64 {
    // AT_EMPTY_PATH + 空路径 = 对 dirfd 本身取状态（等价 fstat）
    if flags & AT_EMPTY_PATH != 0 {
        let empty = match guest_path(m, path_ptr) {
            Ok(p) => p.is_empty(),
            Err(_) => path_ptr == 0,
        };
        if empty {
            return sys_fstat(m, dirfd, out);
        }
    }
    sys_stat_path(m, dirfd, path_ptr, out, flags & AT_SYMLINK_NOFOLLOW == 0)
}

/// `access` / `faccessat` / `faccessat2`。
///
/// **`dirfd` 必须真的用上**：早先分发表把 `faccessat` 直接接到只认 cwd 的
/// `sys_access` 上，`a[0]` 被整个丢掉。表现是相对路径全按当前目录解析——
/// `openat(dirfd, ...)` 读得到的文件，`faccessat(同一个 dirfd, ...)` 却报
/// ENOENT（`t_fd_open` 的 openat/fork-child-reused-dirfd 以 exit 5 抓到）。
fn sys_faccessat(m: &mut Machine, dirfd: i32, path_ptr: u64, _mode: i32) -> i64 {
    let path = match guest_path(m, path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if fs::DevKind::from_guest_path(&path).is_some() {
        return 0;
    }
    let host = match resolve_at(m, dirfd, &path, true) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // 只做存在性判断。真实的 X_OK/W_OK 要看宿主权限位，Windows 上没有
    // 对应语义；容器内一律 root，存在即可访问是可接受的近似。
    if host.exists() {
        0
    } else {
        -ENOENT
    }
}

fn sys_unlinkat(m: &mut Machine, dirfd: i32, path_ptr: u64, flags: i32) -> i64 {
    let path = match guest_path(m, path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if m.os.vfs.is_readonly(&path) {
        return -EROFS;
    }
    // unlink/rmdir 删的是**链接本身**，绝不能跟随末段。
    let host = match resolve_at(m, dirfd, &path, false) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let r = if flags & AT_REMOVEDIR != 0 {
        std::fs::remove_dir(&host)
    } else {
        std::fs::remove_file(&host)
    };
    match r {
        Ok(()) => 0,
        Err(e) => host_err(&e),
    }
}

fn sys_readlinkat(m: &mut Machine, dirfd: i32, path_ptr: u64, buf: u64, size: u64) -> i64 {
    let path = match guest_path(m, path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // /proc/self/exe 是 guest 定位自身（然后 re-exec 或读自己的 ELF）的常用
    // 手段。procfs 整体还没合成，但这一条必须给**真的路径**：回一个
    // "/proc/self/exe" 自身会让 guest 拿到一条 exec 不出去的路径，
    // 表现成 execl 无声失败——比直接 EINVAL 难查得多。
    if path == "/proc/self/exe" || path == format!("/proc/{}/exe", m.os.pid) {
        if m.os.exe.is_empty() {
            return -ENOENT;
        }
        let s = m.os.exe.clone().into_bytes();
        let n = (size as usize).min(s.len());
        return if m.mem.write(buf, &s[..n]).is_err() {
            -EFAULT
        } else {
            n as i64
        };
    }
    // readlink 读的就是链接本身，跟随了就什么都读不到。
    let host = match resolve_at(m, dirfd, &path, false) {
        Ok(p) => p,
        Err(e) => return e,
    };
    match std::fs::read_link(&host) {
        Ok(target) => {
            let s = target.to_string_lossy().into_owned().into_bytes();
            let n = (size as usize).min(s.len());
            if m.mem.write(buf, &s[..n]).is_err() {
                -EFAULT
            } else {
                n as i64
            }
        }
        Err(e) => host_err(&e),
    }
}

fn sys_getcwd(m: &mut Machine, buf: u64, size: u64) -> i64 {
    let s = m.os.vfs.cwd.to_string_lossy().into_owned();
    let mut b = s.into_bytes();
    b.push(0);
    if (b.len() as u64) > size {
        return -EINVAL; // Linux 用 ERANGE(34)，但 getcwd 的手册说缓冲不足是 ERANGE
    }
    if m.mem.write(buf, &b).is_err() {
        return -EFAULT;
    }
    b.len() as i64
}

fn sys_chdir(m: &mut Machine, path_ptr: u64) -> i64 {
    let path = match guest_path(m, path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // cwd 的记法随模式不同（容器模式记 guest 视角，直通模式记宿主路径），
    // 必须走 cwd_for 而不是直接 normalize——否则 Windows 直通模式下盘符会丢。
    let next = m.os.vfs.cwd_for(&path);
    let host = m.os.vfs.host_path(&path);
    if !host.is_dir() {
        return if host.exists() { -ENOTDIR } else { -ENOENT };
    }
    m.os.vfs.cwd = next;
    0
}

// ------------------------------------------------------------- 内存

const MAP_FIXED: i32 = 0x10;
const MAP_ANONYMOUS: i32 = 0x20;
const MAP_SHARED: i32 = 0x01;

fn prot_from_guest(p: i32) -> u8 {
    let mut o = 0;
    if p & 1 != 0 {
        o |= PROT_READ;
    }
    if p & 2 != 0 {
        o |= PROT_WRITE;
    }
    if p & 4 != 0 {
        o |= PROT_EXEC;
    }
    o
}

fn sys_mmap(m: &mut Machine, addr: u64, len: u64, prot: i32, flags: i32, fd: i32, off: i64) -> i64 {
    if len == 0 {
        return -EINVAL;
    }
    let len = (len + PAGE_MASK) & !PAGE_MASK;
    let base = if flags & MAP_FIXED != 0 {
        if addr & PAGE_MASK != 0 {
            return -EINVAL;
        }
        addr
    } else if addr != 0 && !m.mem.is_mapped(addr, len) {
        // 有 hint 且该区间空闲：照 hint 放（Linux 的行为）
        addr & !PAGE_MASK
    } else {
        m.mem.find_free(len)
    };

    // MAP_FIXED 会**覆盖**目标区间上已有的映射。那上面若是共享文件映射，
    // 覆盖之前必须先刷回——否则那段写入随覆盖一起消失，而 guest 完全看不出
    // 发生过什么。刷不动就整条 mmap 失败并保持原样（`t_mmap` 的
    // `--writeback-failure-fixed` 断言的正是"报 EIO 且内容还在"）。
    if flags & MAP_FIXED != 0 {
        if let Err(e) = flush_file_maps(m, base, len) {
            return e;
        }
        if let Err(e) = split_file_maps(m, base, len) {
            return e;
        }
    }

    // guest 请求的 prot 可能不含写；但文件映射要先把内容写进去，
    // 所以先按可写建立，填完再收紧。
    m.mem.map(base, len, PROT_READ | PROT_WRITE);
    m.mem.zero(base, len);

    if flags & MAP_ANONYMOUS == 0 {
        // 文件映射：先把内容读进来，再把这段映射**记账**（见 `FileMap`）。
        // 记账是 MAP_SHARED 能回写的前提；没有它，写入只改了引擎自己的页表，
        // 宿主文件毫不知情。
        let mut buf = vec![0u8; len as usize];
        // 句柄要 `try_clone` 一份自己留着：guest 随后关掉它那个 fd 之后，
        // 映射依然有效（POSIX 明确规定），回写也还得找得到文件。
        let (got, own) = match m.os.fds.get_mut(fd).map(|f| &mut f.kind) {
            Some(FdKind::File(f)) => {
                let cur = f.stream_position().ok();
                let r = f
                    .seek(SeekFrom::Start(off as u64))
                    .and_then(|_| read_up_to(f, &mut buf));
                if let Some(c) = cur {
                    let _ = f.seek(SeekFrom::Start(c));
                }
                (r, f.try_clone().ok())
            }
            Some(_) | None => {
                m.mem.unmap(base, len);
                return -EBADF;
            }
        };
        match got {
            Ok(n) => {
                let data = buf[..n].to_vec();
                m.mem.write_raw(base, &data);
            }
            Err(e) => {
                m.mem.unmap(base, len);
                return host_err(&e);
            }
        }
        if let Some(file) = own {
            // 同一段地址重新映射时，旧记账要先去掉。
            let _ = split_file_maps(m, base, len);
            m.os.file_maps.push(FileMap {
                base,
                len,
                file,
                offset: off as u64,
                shared: flags & MAP_SHARED != 0,
            });
        }
    }

    m.mem.map(base, len, prot_from_guest(prot));
    base as i64
}

/// 读满 `buf` 或到 EOF。`Read::read` 允许短读，直接用会漏内容。
fn read_up_to(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

fn sys_mprotect(m: &mut Machine, addr: u64, len: u64, prot: i32) -> i64 {
    if addr & PAGE_MASK != 0 {
        return -EINVAL;
    }
    let len = (len + PAGE_MASK) & !PAGE_MASK;
    if !m.mem.is_mapped(addr, len) {
        return -ENOMEM;
    }
    m.mem.map(addr, len, prot_from_guest(prot));
    0
}

/// 测试用的回写故障注入。
///
/// 回写失败**必须能被 guest 观察到**——`msync`/`munmap` 报 `EIO` 且映射保持
/// 原样，让调用方有机会重试。真机上这条路要磁盘满/IO 错才走得到，没法在
/// 门禁里稳定复现，所以留一个只认环境变量的注入点。`t_mmap` 的
/// `--writeback-failure-*` 五个探针跑的就是它。
fn fshare_fault() -> Option<String> {
    std::env::var("WBOX_TEST_FSHARE_FAIL").ok()
}

thread_local! {
    /// 回写尝试计数（注入点按第几次来决定失败哪一次）。
    static FLUSH_ATTEMPT: Cell<u32> = const { Cell::new(0) };
    /// 拆分尝试计数。
    static SPLIT_ATTEMPT: Cell<u32> = const { Cell::new(0) };
}

/// 这一次回写是否要被注入成失败。
fn inject_flush_failure() -> bool {
    let Some(kind) = fshare_fault() else {
        return false;
    };
    let n = FLUSH_ATTEMPT.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    });
    match kind.as_str() {
        "writeback" => n == 0,
        "writeback-second" => n == 1,
        _ => false,
    }
}

fn inject_split_failure() -> bool {
    if fshare_fault().as_deref() != Some("split") {
        return false;
    }
    SPLIT_ATTEMPT.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v == 0
    })
}

/// 把落在 `[addr, addr+len)` 内的**共享**文件映射刷回文件。
///
/// `addr == 0 && len == 0` 表示"全部刷"，用于进程退出与 `execve`。
///
/// 失败返回负 errno，且**一个字节都不落**——调用方据此保留映射并把错误交给
/// guest。刷了一半再报错是最糟的：文件处于中间状态，而调用方以为什么都没写。
fn flush_file_maps(m: &mut Machine, addr: u64, len: u64) -> Result<(), i64> {
    let all = addr == 0 && len == 0;
    let maps: Vec<(u64, u64, u64, usize)> =
        m.os.file_maps
            .iter()
            .enumerate()
            .filter(|(_, fm)| fm.shared)
            .filter_map(|(i, fm)| {
                let (s, e) = (fm.base, fm.base + fm.len);
                let (rs, re) = if all { (s, e) } else { (addr, addr + len) };
                let lo = s.max(rs);
                let hi = e.min(re);
                if lo >= hi {
                    None
                } else {
                    Some((lo, hi - lo, fm.offset + (lo - s), i))
                }
            })
            .collect();
    if maps.is_empty() {
        return Ok(());
    }
    if inject_flush_failure() {
        return Err(-EIO);
    }
    for (start, n, off, i) in maps {
        // 用 `read_raw`：guest 可能先 `mprotect(PROT_NONE)` 再 `munmap`，
        // 而内核的页缓存不会因为映射不可读就丢内容。按 PROT_READ 去读会失败，
        // 那段写入就静默丢了。
        let mut buf = vec![0u8; n as usize];
        m.mem.read_raw(start, &mut buf);
        let fm = &mut m.os.file_maps[i];
        let r = fm.file.seek(SeekFrom::Start(off)).and_then(|_| {
            use std::io::Write as _;
            fm.file.write_all(&buf)
        });
        if let Err(e) = r {
            return Err(host_err(&e));
        }
    }
    Ok(())
}

/// 按 `[addr, addr+len)` 裁剪映射记账。
///
/// 三种形态：整段被吃掉（删记录）、只切掉一头（改 base/len/offset）、
/// **从中间挖掉一块**（要拆成两条记录，因此需要再复制一份文件句柄——
/// 句柄用尽时如实报 `EMFILE` 并保持原样，而不是悄悄丢掉一半的回写能力）。
fn split_file_maps(m: &mut Machine, addr: u64, len: u64) -> Result<(), i64> {
    let end = addr + len;
    let mut add: Vec<FileMap> = Vec::new();
    let mut drop_idx: Vec<usize> = Vec::new();
    for i in 0..m.os.file_maps.len() {
        let (b, l, off) = {
            let fm = &m.os.file_maps[i];
            (fm.base, fm.len, fm.offset)
        };
        let (s, e) = (b, b + l);
        if end <= s || addr >= e {
            continue;
        }
        let head = addr > s;
        let tail = end < e;
        if head && tail {
            if inject_split_failure() {
                return Err(-EMFILE);
            }
            let Ok(dup) = m.os.file_maps[i].file.try_clone() else {
                return Err(-EMFILE);
            };
            add.push(FileMap {
                base: end,
                len: e - end,
                file: dup,
                offset: off + (end - s),
                shared: m.os.file_maps[i].shared,
            });
            m.os.file_maps[i].len = addr - s;
        } else if head {
            m.os.file_maps[i].len = addr - s;
        } else if tail {
            m.os.file_maps[i].base = end;
            m.os.file_maps[i].len = e - end;
            m.os.file_maps[i].offset = off + (end - s);
        } else {
            drop_idx.push(i);
        }
    }
    for i in drop_idx.into_iter().rev() {
        m.os.file_maps.remove(i);
    }
    m.os.file_maps.extend(add);
    Ok(())
}

fn sys_munmap(m: &mut Machine, addr: u64, len: u64) -> i64 {
    if addr & PAGE_MASK != 0 {
        return -EINVAL;
    }
    let len = (len + PAGE_MASK) & !PAGE_MASK;
    // **先刷回再解除映射**：解除之后页表就读不到内容了。
    // 刷不动就整条 `munmap` 失败并保持映射原样——guest 才有机会重试。
    if let Err(e) = flush_file_maps(m, addr, len) {
        return e;
    }
    if let Err(e) = split_file_maps(m, addr, len) {
        return e;
    }
    m.mem.unmap(addr, len);
    0
}

/// 调整某段文件映射的记账长度（`mremap` 原地伸缩后）。
fn resize_file_map(m: &mut Machine, base: u64, new_len: u64) {
    if let Some(fm) = m.os.file_maps.iter_mut().find(|f| f.base == base) {
        fm.len = new_len;
    }
}

/// `mremap` 就地扩大文件映射时，把新增的那一段从文件补读进来。
fn fill_grown_file_map(m: &mut Machine, base: u64, old_len: u64, grow: u64) {
    let Some(i) = m.os.file_maps.iter().position(|f| f.base == base) else {
        return;
    };
    let off = m.os.file_maps[i].offset + old_len;
    let mut buf = vec![0u8; grow as usize];
    let fm = &mut m.os.file_maps[i];
    let n = match fm
        .file
        .seek(SeekFrom::Start(off))
        .and_then(|_| read_up_to(&mut fm.file, &mut buf))
    {
        Ok(n) => n,
        Err(_) => return,
    };
    let data = buf[..n].to_vec();
    m.mem.write_raw(base + old_len, &data);
}

const MREMAP_MAYMOVE: i32 = 1;
const MREMAP_FIXED: i32 = 2;

fn sys_mremap(
    m: &mut Machine,
    old: u64,
    old_len: u64,
    new_len: u64,
    flags: i32,
    new_addr: u64,
) -> i64 {
    if old & PAGE_MASK != 0 || new_len == 0 {
        return -EINVAL;
    }
    // 只认这两个 flag；其余位（guest 会拿 0x40000000 之类来探边界）必须 EINVAL，
    // 不能当成 0 悄悄接受。MREMAP_FIXED 还要求同时给 MAYMOVE。
    const KNOWN: i32 = MREMAP_MAYMOVE | MREMAP_FIXED;
    if flags & !KNOWN != 0 {
        return -EINVAL;
    }
    if flags & MREMAP_FIXED != 0 && flags & MREMAP_MAYMOVE == 0 {
        return -EINVAL;
    }
    let old_len = (old_len + PAGE_MASK) & !PAGE_MASK;
    let new_len = (new_len + PAGE_MASK) & !PAGE_MASK;
    if !m.mem.is_mapped(old, old_len) {
        return -EFAULT;
    }
    // 缩小/等长：原地截掉尾部（截掉的那段若是共享映射要先刷回）。
    //
    // **但 MREMAP_FIXED 不走这条**：它要求落在指定地址上，哪怕长度没变。
    // 漏掉这个条件的表现是 `mremap(src, N, N, MAYMOVE|FIXED, target)` 原样
    // 返回 src，调用方拿到的地址根本不是它指定的那个
    // （`mremap/file-fixed-replaces-shared-target` 抓的就是这条）。
    if new_len <= old_len && flags & MREMAP_FIXED == 0 {
        if let Err(e) = flush_file_maps(m, old + new_len, old_len - new_len) {
            return e;
        }
        m.mem.unmap(old + new_len, old_len - new_len);
        resize_file_map(m, old, new_len);
        return old as i64;
    }
    // 扩大：先试原地
    let grow = new_len - old_len;
    if flags & MREMAP_FIXED == 0 && !m.mem.is_mapped(old + old_len, grow) {
        m.mem.map(old + old_len, grow, PROT_READ | PROT_WRITE);
        m.mem.zero(old + old_len, grow);
        // **扩大的那段要从文件补读**。文件映射扩大后，新增页对应的是文件里
        // 更后面的内容，不是零页——`mremap/file-private-readback` 正是先缩到
        // 一页再逐步扩回去，然后断言第 2、3 页读到的仍是文件内容。
        fill_grown_file_map(m, old, old_len, grow);
        resize_file_map(m, old, new_len);
        return old as i64;
    }
    if flags & (MREMAP_MAYMOVE | MREMAP_FIXED) == 0 {
        return -ENOMEM;
    }
    // 搬移：拷内容到新地址
    let dst = if flags & MREMAP_FIXED != 0 {
        new_addr
    } else {
        m.mem.find_free(new_len)
    };
    let mut buf = vec![0u8; old_len as usize];
    if m.mem.read(old, &mut buf).is_err() {
        return -EFAULT;
    }
    // 目标区间可能压着别的映射（MREMAP_FIXED 的语义就是替换它）：
    // 先把那边的共享内容刷回去再覆盖，否则那段写入直接丢了。
    if let Err(e) = flush_file_maps(m, dst, new_len) {
        return e;
    }
    if let Err(e) = split_file_maps(m, dst, new_len) {
        return e;
    }
    m.mem.map(dst, new_len, PROT_READ | PROT_WRITE);
    m.mem.zero(dst, new_len);
    m.mem.write_raw(dst, &buf);
    m.mem.unmap(old, old_len);
    // 记账跟着搬：基址与长度都变了，文件与偏移不变。
    if let Some(fm) = m.os.file_maps.iter_mut().find(|f| f.base == old) {
        fm.base = dst;
        fm.len = new_len;
    }
    // 搬完再补读扩大的那一段（要在记账更新之后，它按新基址找）。
    if new_len > old_len {
        fill_grown_file_map(m, dst, old_len, new_len - old_len);
    }
    dst as i64
}

fn sys_brk(m: &mut Machine, want: u64) -> i64 {
    let cur = m.mem.brk;
    if want == 0 || want < m.mem.brk_start {
        return cur as i64;
    }
    if want > cur {
        let start = (cur + PAGE_MASK) & !PAGE_MASK;
        let end = (want + PAGE_MASK) & !PAGE_MASK;
        if end > start {
            m.mem.map(start, end - start, PROT_READ | PROT_WRITE);
            m.mem.zero(start, end - start);
        }
    } else {
        let from = (want + PAGE_MASK) & !PAGE_MASK;
        let to = (cur + PAGE_MASK) & !PAGE_MASK;
        if to > from {
            m.mem.unmap(from, to - from);
        }
    }
    m.mem.brk = want;
    want as i64
}

// ------------------------------------------------------------- 杂项

fn sys_arch_prctl(m: &mut Machine, code: i32, addr: u64) -> i64 {
    const ARCH_SET_GS: i32 = 0x1001;
    const ARCH_SET_FS: i32 = 0x1002;
    const ARCH_GET_FS: i32 = 0x1003;
    const ARCH_GET_GS: i32 = 0x1004;
    match code {
        ARCH_SET_FS => {
            m.cpu.fs_base = addr;
            0
        }
        ARCH_SET_GS => {
            m.cpu.gs_base = addr;
            0
        }
        ARCH_GET_FS => {
            let v = m.cpu.fs_base;
            if m.mem.write_u64(addr, v).is_err() {
                -EFAULT
            } else {
                0
            }
        }
        ARCH_GET_GS => {
            let v = m.cpu.gs_base;
            if m.mem.write_u64(addr, v).is_err() {
                -EFAULT
            } else {
                0
            }
        }
        _ => -EINVAL,
    }
}

/// `struct utsname`：6 个 65 字节字段。
fn sys_uname(m: &mut Machine, out: u64) -> i64 {
    let mut b = [0u8; 65 * 6];
    let fields = [
        "Linux",
        "wbox",
        m.os.release.as_str(),
        "#1 SMP wbox",
        "x86_64",
        "(none)",
    ];
    for (i, s) in fields.iter().enumerate() {
        let bytes = s.as_bytes();
        let n = bytes.len().min(64);
        b[i * 65..i * 65 + n].copy_from_slice(&bytes[..n]);
    }
    if m.mem.write(out, &b).is_err() {
        -EFAULT
    } else {
        0
    }
}

/// 读一次 timerfd：返回到期次数，0 = 还没到期。
///
/// **阻塞模式下要真的等到到期**。单线程里没有别的执行体能推进时间，但时间
/// 本身会走——所以这里睡到 deadline 再结算是正确的，也是 guest 唯一能拿到
/// "等定时器"语义的地方（`timerfd/fork-shares-description` 的子进程就是
/// fork 完直接阻塞读）。
fn timer_read(t: &Rc<fs::TimerFd>, nonblock: bool) -> u64 {
    let n = t.take(now_ns());
    if n > 0 || nonblock {
        return n;
    }
    let dl = t.deadline_ns.get();
    if dl == 0 {
        return 0; // 未武装：永远不会到期，阻塞等于挂死，直接报 EAGAIN
    }
    let wait = dl.saturating_sub(now_ns());
    // 上限兜底：不让一个远期定时器把整个 guest 挂住。
    let wait = wait.min(5_000_000_000);
    std::thread::sleep(std::time::Duration::from_nanos(wait));
    t.take(now_ns())
}

/// 当前时刻的纳秒数。
///
/// **所有时钟走同一个源**：`clock_gettime` 本来就忽略 `clockid`，timerfd 的
/// 绝对超时（`TFD_TIMER_ABSTIME`）要和 guest 自己读到的时钟对得上，两边用
/// 不同的源就会算错到期时刻。
pub fn now_ns() -> u64 {
    let (s, ns) = now();
    s.saturating_mul(1_000_000_000).saturating_add(ns as u64)
}

fn now() -> (u64, u32) {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs(), d.subsec_nanos()))
        .unwrap_or((0, 0))
}

fn sys_time(m: &mut Machine, out: u64) -> i64 {
    let (s, _) = now();
    if out != 0 && m.mem.write_u64(out, s).is_err() {
        return -EFAULT;
    }
    s as i64
}

fn sys_gettimeofday(m: &mut Machine, tv: u64, _tz: u64) -> i64 {
    if tv == 0 {
        return 0;
    }
    let (s, ns) = now();
    if m.mem.write_u64(tv, s).is_err() || m.mem.write_u64(tv + 8, (ns / 1000) as u64).is_err() {
        return -EFAULT;
    }
    0
}

fn sys_clock_gettime(m: &mut Machine, _clk: i32, ts: u64) -> i64 {
    if ts == 0 {
        return -EFAULT;
    }
    let (s, ns) = now();
    if m.mem.write_u64(ts, s).is_err() || m.mem.write_u64(ts + 8, ns as u64).is_err() {
        return -EFAULT;
    }
    0
}

const RLIMIT_STACK: u64 = 3;
const RLIMIT_NOFILE: u64 = 7;
/// Linux 的 `RLIM_NLIMITS`。超出即 `EINVAL`。
const RLIM_NLIMITS: u64 = 16;
const RLIM_INFINITY: u64 = u64::MAX;

fn current_rlimit(m: &Machine, res: u64) -> (u64, u64) {
    match res {
        RLIMIT_STACK => (crate::stack::STACK_SIZE, crate::stack::STACK_SIZE),
        RLIMIT_NOFILE => (m.os.fds.nofile(), fs::MAX_NOFILE),
        _ => (RLIM_INFINITY, RLIM_INFINITY),
    }
}

/// `getrlimit` / `setrlimit` / `prlimit64`。
///
/// # 参数校验的顺序是有讲究的
///
/// `prlimit64(pid, res, new, old)` 在 Linux 上：**先读 `new`**（读不动就
/// `EFAULT`，且 `old` 一个字节都不写），再校验 `res`，再校验 `cur <= max`，
/// 应用之后**才**写 `old`——所以"`old` 指针是坏的"这一条会返回 `EFAULT`
/// 但**新限额已经生效**。这不是随手定的顺序，`t_negative` 的
/// prlimit-bad-new-preserves-old 与 prlimit-bad-old-applies-new 分别钉死了
/// 两头，任何一头搞反都会被抓到。
fn sys_rlimit(m: &mut Machine, nr: u64, a: &[u64; 6]) -> i64 {
    // getrlimit(res, out) / setrlimit(res, new) / prlimit64(pid, res, new, out)
    let (res, new, out) = match nr {
        97 => (a[0], 0, a[1]),
        160 => (a[0], a[1], 0),
        _ => (a[1], a[2], a[3]),
    };

    if new != 0 {
        let (cur, max) = match (m.mem.read_u64(new), m.mem.read_u64(new + 8)) {
            (Ok(c), Ok(x)) => (c, x),
            _ => return -EFAULT,
        };
        if res >= RLIM_NLIMITS {
            return -EINVAL;
        }
        // 软限额不得高于硬限额；也不得抬高硬限额（非特权进程的规矩）。
        let (_, hard) = current_rlimit(m, res);
        if cur > max || max > hard {
            return -EINVAL;
        }
        if res == RLIMIT_NOFILE {
            m.os.fds.set_nofile(cur);
        }
        // 其余资源只做校验、不真的生效——如实说明好过假装。
    } else if res >= RLIM_NLIMITS {
        return -EINVAL;
    }

    if out == 0 {
        return 0;
    }
    let (cur, max) = current_rlimit(m, res);
    if m.mem.write_u64(out, cur).is_err() || m.mem.write_u64(out + 8, max).is_err() {
        return -EFAULT;
    }
    0
}

/// `getrandom`。一定要走宿主真正的 CSPRNG。
///
/// 这里绝不能退化成可预测的伪随机：guest 用它做栈保护 canary、哈希种子和
/// 密钥生成，给一个可猜的序列等于静默削弱 guest 的安全性，而且不会有任何
/// 报错提示出了问题。宁可报错也不给假随机。
fn sys_getrandom(m: &mut Machine, buf: u64, len: u64, _flags: u32) -> i64 {
    let n = len.min(1 << 20) as usize;
    let mut tmp = vec![0u8; n];
    if let Err(e) = host_random(&mut tmp) {
        return e;
    }
    if m.mem.write(buf, &tmp).is_err() {
        return -EFAULT;
    }
    n as i64
}

/// 用宿主 CSPRNG 填满 `out`。失败时返回 `-errno`。
#[cfg(unix)]
fn host_random(out: &mut [u8]) -> Result<(), i64> {
    // `/dev/urandom` 在所有 Unix 上都可用，不需要 libc 的 getrandom 包装。
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(out))
        .map_err(|e| host_err(&e))
}

#[cfg(windows)]
fn host_random(out: &mut [u8]) -> Result<(), i64> {
    use windows_sys::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };
    // hAlgorithm = null + BCRYPT_USE_SYSTEM_PREFERRED_RNG 表示用系统首选 RNG，
    // 不需要先 BCryptOpenAlgorithmProvider。
    // SAFETY：out 是有效可写切片，长度按字节传给 API。
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            out.as_mut_ptr(),
            out.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(-EIO)
    }
}

#[cfg(not(any(unix, windows)))]
fn host_random(_out: &mut [u8]) -> Result<(), i64> {
    Err(-ENOSYS)
}

#[cfg(test)]
mod tests;

// ------------------------------------------------- 文件系统写操作补齐
//
// 这一批是 guest 侧最常用、实现成本又最低的一组。缺了它们的表现很误导：
// `mkdir` 返回 ENOSYS 后，后续所有对该目录的写入都报 ENOENT，看起来像
// 路径解析坏了，实际只是目录压根没建起来（guest C 套件当初 20/21 红就是这样）。

fn sys_mkdir(m: &mut Machine, dirfd: i32, path_ptr: u64) -> i64 {
    let path = match guest_path(m, path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if m.os.vfs.is_readonly(&path) {
        return -EROFS;
    }
    // mkdir 的末段还不存在；若已存在（哪怕是符号链接）应报 EEXIST，
    // 跟随会跑去目标位置建目录——那是错的。
    let host = match resolve_at(m, dirfd, &path, false) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // 只建最后一级：`mkdir` 的语义是父目录必须已存在，
    // 用 create_dir_all 会把 guest 的错误处理路径悄悄抹掉。
    match std::fs::create_dir(&host) {
        Ok(()) => 0,
        Err(e) => host_err(&e),
    }
}

fn sys_rename(m: &mut Machine, odirfd: i32, old_ptr: u64, ndirfd: i32, new_ptr: u64) -> i64 {
    let (old, new) = match (guest_path(m, old_ptr), guest_path(m, new_ptr)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    // 两端**任意一端**在只读挂载下都不行：搬出去要删源，搬进来要建目标。
    if m.os.vfs.is_readonly(&old) || m.os.vfs.is_readonly(&new) {
        return -EROFS;
    }
    // rename 搬的是**链接本身**，两端都不跟随末段。
    let (oh, nh) = match (
        resolve_at(m, odirfd, &old, false),
        resolve_at(m, ndirfd, &new, false),
    ) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    match std::fs::rename(&oh, &nh) {
        Ok(()) => 0,
        Err(e) => host_err(&e),
    }
}

/// `link` / `linkat`。
///
/// **两个 `dirfd` 都要用上**：早先分发表把 `linkat` 接到只认 cwd 的
/// `sys_link` 上，`a[0]`/`a[2]` 被丢掉，相对路径全按当前目录解析
/// （`t_fd_open` 的 openat/fork-child-reused-dirfd 以 exit 7 抓到）。
fn sys_linkat(
    m: &mut Machine,
    odirfd: i32,
    old_ptr: u64,
    ndirfd: i32,
    new_ptr: u64,
    flags: i32,
) -> i64 {
    const AT_SYMLINK_FOLLOW: i32 = 0x400;
    let (old, new) = match (guest_path(m, old_ptr), guest_path(m, new_ptr)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    // 新名字要建在只读挂载里：EROFS。
    if m.os.vfs.is_readonly(&new) {
        return -EROFS;
    }
    // link(2) 默认不跟随末段：老路径是链接就硬链到链接本身，除非显式给了
    // `AT_SYMLINK_FOLLOW`。新路径**永远**不跟随——跟随会跑去目标位置建链接。
    let oh = match resolve_at(m, odirfd, &old, flags & AT_SYMLINK_FOLLOW != 0) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let nh = match resolve_at(m, ndirfd, &new, false) {
        Ok(p) => p,
        Err(e) => return e,
    };
    match std::fs::hard_link(&oh, &nh) {
        Ok(()) => 0,
        Err(e) => host_err(&e),
    }
}

/// `symlink`。目标字符串**原样**写入，不做 VFS 翻译——guest 视角的
/// symlink 目标就该是 guest 路径，翻译了反而会把宿主路径漏进 rootfs。
fn sys_symlink(m: &mut Machine, target_ptr: u64, link_ptr: u64) -> i64 {
    let (target, link) = match (guest_path(m, target_ptr), guest_path(m, link_ptr)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    // 要创建的链接名，绝不跟随末段——跟随会把已存在的同名链接解开，
    // 于是在它的目标位置建链接。
    let link_host = m.os.vfs.host_path_nofollow(&link);
    #[cfg(unix)]
    {
        match std::os::unix::fs::symlink(&target, &link_host) {
            Ok(()) => 0,
            Err(e) => host_err(&e),
        }
    }
    #[cfg(windows)]
    {
        // Windows 建 symlink 默认要开发者模式或管理员权限。失败时如实报
        // EPERM，不要伪造成功——guest 随后读这个链接会得到更难查的错误。
        // 这里探测的是**目标**是不是目录（Windows 建链接要分 dir/file），
        // 跟随末段才能问到真实类型。
        let r = if m.os.vfs.host_path(&target).is_dir() {
            std::os::windows::fs::symlink_dir(&target, &link_host)
        } else {
            std::os::windows::fs::symlink_file(&target, &link_host)
        };
        match r {
            Ok(()) => 0,
            Err(e) => host_err(&e),
        }
    }
}

/// `mount(source, target, fstype, flags, data)`。
///
/// 只支持 `hostfs`——引擎里"文件系统"只有一种：宿主目录。别的类型如实报
/// `ENODEV` 而不是假装挂上了。
fn sys_mount(m: &mut Machine, src: u64, tgt: u64, fstype: u64, flags: u64, _data: u64) -> i64 {
    const MS_RDONLY: u64 = 1;
    let (src, tgt, fst) = match (
        guest_path(m, src),
        guest_path(m, tgt),
        guest_path(m, fstype),
    ) {
        (Ok(a), Ok(b), Ok(c)) => (a, b, c),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return e,
    };
    if fst != "hostfs" && fst != "bind" && fst != "none" {
        return -ENODEV;
    }
    let source = match m.os.vfs.host_path_confined(&src) {
        Ok(p) => p,
        Err(e) => return e.errno(),
    };
    if !source.is_dir() {
        return -ENOTDIR;
    }
    // 挂载点必须已存在（Linux 如此）。
    let tgt_host = match m.os.vfs.host_path_confined(&tgt) {
        Ok(p) => p,
        Err(e) => return e.errno(),
    };
    if !tgt_host.is_dir() {
        return -ENOTDIR;
    }
    let target = m.os.vfs.guest_segments(&tgt);
    if target.is_empty() {
        return -EINVAL; // 不允许挂到 guest 根上：那会把 rootfs 整个换掉
    }
    m.os.vfs.mounts.push(fs::Mount {
        target,
        source,
        readonly: flags & MS_RDONLY != 0,
    });
    0
}

/// 路径落在只读挂载下就报 `EROFS`，否则成功。
fn sys_readonly_guard(m: &mut Machine, path_ptr: u64) -> i64 {
    match guest_path(m, path_ptr) {
        Ok(p) if m.os.vfs.is_readonly(&p) => -EROFS,
        Ok(_) => 0,
        Err(e) => e,
    }
}

fn sys_readonly_guard_at(m: &mut Machine, _dirfd: i32, path_ptr: u64) -> i64 {
    sys_readonly_guard(m, path_ptr)
}

fn sys_truncate(m: &mut Machine, path_ptr: u64, len: i64) -> i64 {
    let path = match guest_path(m, path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if m.os.vfs.is_readonly(&path) {
        return -EROFS;
    }
    if len < 0 {
        return -EINVAL;
    }
    let host = m.os.vfs.host_path(&path);
    match std::fs::OpenOptions::new().write(true).open(&host) {
        Ok(f) => match f.set_len(len as u64) {
            Ok(()) => 0,
            Err(e) => host_err(&e),
        },
        Err(e) => host_err(&e),
    }
}

fn sys_ftruncate(m: &mut Machine, fd: i32, len: i64) -> i64 {
    if len < 0 {
        return -EINVAL;
    }
    match m.os.fds.get(fd).map(|f| &f.kind) {
        Some(FdKind::File(f)) => match f.set_len(len as u64) {
            Ok(()) => 0,
            Err(e) => host_err(&e),
        },
        Some(_) => -EINVAL,
        None => -EBADF,
    }
}

fn sys_fsync(m: &mut Machine, fd: i32) -> i64 {
    match m.os.fds.get(fd).map(|f| &f.kind) {
        Some(FdKind::File(f)) => match f.sync_all() {
            Ok(()) => 0,
            Err(e) => host_err(&e),
        },
        // **管道没有 fsync 语义**：Linux 对管道/FIFO/socket 返回 EINVAL，
        // 不是成功。报成功会让"把数据刷到持久存储"这类判断悄悄走偏，
        // 而调用方拿不到任何提示（`t_fd_rw` 的 pread/negative 判的就是这条）。
        Some(FdKind::PipeRead(_)) | Some(FdKind::PipeWrite(_)) | Some(FdKind::Socket(_)) => -EINVAL,
        // 标准流与合成设备没有"落盘"这回事，报成功。
        Some(_) => 0,
        None => -EBADF,
    }
}

fn sys_fchdir(m: &mut Machine, fd: i32) -> i64 {
    match m.os.fds.get(fd).map(|f| &f.kind) {
        Some(FdKind::Dir { path, .. }) => {
            let p = path.clone();
            m.os.vfs.cwd = p;
            0
        }
        Some(_) => -ENOTDIR,
        None => -EBADF,
    }
}

fn sys_pwrite(m: &mut Machine, fd: i32, buf: u64, count: u64, off: i64) -> i64 {
    if matches!(m.os.fds.get(fd).map(|f| &f.kind), Some(FdKind::Event(_))) {
        return sys_write(m, fd, buf, count);
    }
    let n = count.min(1 << 20) as usize;
    let mut tmp = vec![0u8; n];
    if m.mem.read(buf, &mut tmp).is_err() {
        return -EFAULT;
    }
    match m.os.fds.get_mut(fd).map(|f| &mut f.kind) {
        Some(FdKind::File(f)) => {
            // pwrite 不得改变文件位置
            let cur = match f.stream_position() {
                Ok(c) => c,
                Err(e) => return host_err(&e),
            };
            let r = f
                .seek(SeekFrom::Start(off as u64))
                .and_then(|_| f.write(&tmp));
            let _ = f.seek(SeekFrom::Start(cur));
            match r {
                Ok(k) => k as i64,
                Err(e) => host_err(&e),
            }
        }
        Some(FdKind::PipeRead(_))
        | Some(FdKind::PipeWrite(_))
        | Some(FdKind::Socket(_))
        | Some(FdKind::Dev(_))
        | Some(FdKind::Stdin)
        | Some(FdKind::Stdout)
        | Some(FdKind::Stderr) => -ESPIPE,
        Some(_) => -EBADF,
        None => -EBADF,
    }
}

fn sys_sendfile(m: &mut Machine, out_fd: i32, in_fd: i32, off_ptr: u64, count: u64) -> i64 {
    let n = count.min(1 << 20) as usize;
    let mut tmp = vec![0u8; n];
    // 带 offset 指针时从该偏移读且不动文件位置；否则按当前位置读。
    let explicit_off = if off_ptr != 0 {
        match m.mem.read_u64(off_ptr) {
            Ok(v) => Some(v),
            Err(_) => return -EFAULT,
        }
    } else {
        None
    };
    let got = match m.os.fds.get_mut(in_fd).map(|f| &mut f.kind) {
        Some(FdKind::File(f)) => match explicit_off {
            Some(o) => {
                let cur = f.stream_position().ok();
                let r = f.seek(SeekFrom::Start(o)).and_then(|_| f.read(&mut tmp));
                if let Some(c) = cur {
                    let _ = f.seek(SeekFrom::Start(c));
                }
                r
            }
            None => f.read(&mut tmp),
        },
        Some(_) => return -EINVAL,
        None => return -EBADF,
    };
    let k = match got {
        Ok(k) => k,
        Err(e) => return host_err(&e),
    };
    let written = write_bytes(m, out_fd, &tmp[..k]);
    if written > 0 {
        if let Some(o) = explicit_off {
            if m.mem.write_u64(off_ptr, o + written as u64).is_err() {
                return -EFAULT;
            }
        }
    }
    written
}

fn sys_dup3(m: &mut Machine, old: i32, new: i32, flags: i32) -> i64 {
    // dup3 与 dup2 的唯一差别：old == new 是错误，且可以带 O_CLOEXEC
    if old == new {
        return -EINVAL;
    }
    let r = sys_dup2(m, old, new);
    if r >= 0 && flags & O_CLOEXEC != 0 {
        if let Some(f) = m.os.fds.get_mut(new) {
            f.cloexec = true;
        }
    }
    r
}

/// `pipe` / `pipe2`。进程内缓冲，见 `fs::PipeInner` 的说明。
fn sys_pipe(m: &mut Machine, fds_ptr: u64, flags: i32) -> i64 {
    let (rk, wk) = fs::new_pipe();
    let cloexec = flags & O_CLOEXEC != 0;
    // **要么两个都拿到，要么一个都不留**。半途 EMFILE 却已经占掉一个 fd，
    // 是最难查的那类泄漏：调用方看到失败、以为什么都没发生，fd 却少了一个。
    // 输出缓冲同理，失败时一个字节都不写（`t_negative` 的 pair-atomic 断言的
    // 正是"失败后 pp[0]/pp[1] 保持原值"）。
    let Some(rd) = m.os.fds.alloc(Fd::new(rk, cloexec, flags & !O_CLOEXEC)) else {
        return -EMFILE;
    };
    let Some(wr) = m.os.fds.alloc(Fd::new(wk, cloexec, flags & !O_CLOEXEC)) else {
        m.os.fds.remove(rd);
        return -EMFILE;
    };
    if m.mem.write_u32(fds_ptr, rd as u32).is_err()
        || m.mem.write_u32(fds_ptr + 4, wr as u32).is_err()
    {
        m.os.fds.remove(rd);
        m.os.fds.remove(wr);
        return -EFAULT;
    }
    0
}

/// `poll`。`struct pollfd { int fd; short events; short revents; }`（8 字节）。
///
/// 单线程模拟器里没有"等待"可言：普通文件与标准流永远就绪，管道按缓冲区
/// 是否有数据判断。**不实现阻塞**——真去阻塞在单线程里必然死锁。
fn sys_poll(m: &mut Machine, fds_ptr: u64, nfds: u64, timeout: i32) -> i64 {
    const POLLERR: u16 = 0x008;
    const POLLHUP: u16 = 0x010;
    const POLLNVAL: u16 = 0x020;
    if nfds > 1024 {
        return -EINVAL;
    }
    let mut ready = 0i64;
    for i in 0..nfds {
        let base = fds_ptr + i * 8;
        let fd = match m.mem.read_u32(base) {
            Ok(v) => v as i32,
            Err(_) => return -EFAULT,
        };
        let events = match m.mem.read_u16(base + 4) {
            Ok(v) => v,
            Err(_) => return -EFAULT,
        };
        // 就绪判定与 epoll 共用 `readiness`——两处分叉的表现是同一个 fd
        // 在 poll 里可读、在 epoll 里不可读，那种不一致极难从现象追回根因。
        //
        // POLLHUP / POLLERR / POLLNVAL **不受 events 掩码约束**：POSIX 规定
        // 它们无论请求与否都会被报出来。早先整条都按 `& events` 过滤，于是
        // "写端已关"这件事对只订阅了 POLLIN 的调用方永远不可见。
        //
        // POLL* 与 EPOLL* 在 Linux 上是同一套位（IN=1/OUT=4/ERR=8/HUP=0x10/
        // RDHUP=0x2000），所以这里直接按位取，不需要翻译表。
        let revents = if !m.os.fds.contains(fd) {
            POLLNVAL
        } else {
            let ready = readiness(m, fd) as u16;
            ready & (events | POLLERR | POLLHUP)
        };
        if m.mem.write_u16(base + 6, revents).is_err() {
            return -EFAULT;
        }
        if revents != 0 {
            ready += 1;
        }
    }
    // 一个都不就绪时**要真的把超时睡满**。单线程下没人能在这期间改变就绪
    // 状态，所以睡完结果一样——但 guest 会用 `poll(NULL, 0, ms)` 当精确
    // 睡眠、也会按"poll 返回得太快"判断自己算错了超时。立刻返回 0 是在
    // 撒谎（`t_net_sockopt` 的 poll/timeout-precision 直接量了这段墙钟）。
    if ready == 0 && timeout != 0 {
        // 等待期间可能有 fd 自己变就绪（timerfd），所以要重新扫一遍并回写。
        // **唤醒条件要带上 events 掩码**。不带的话，一个只订阅了 POLLIN 的
        // socketpair 读端会因为"可写"而立刻唤醒——`poll(fd, POLLIN, 200)`
        // 于是几毫秒就返回 0，而 guest 正拿它当精确睡眠
        // （`t_net_sockopt` 的 poll/timeout-precision 量的就是这段墙钟）。
        let woke = wait_until_ready(timeout, || {
            (0..nfds).any(|i| {
                let Ok(fd) = m.mem.read_u32(fds_ptr + i * 8) else {
                    return false;
                };
                let Ok(ev) = m.mem.read_u16(fds_ptr + i * 8 + 4) else {
                    return false;
                };
                readiness(m, fd as i32) as u16 & (ev | POLLERR | POLLHUP) != 0
            })
        });
        if woke {
            return sys_poll(m, fds_ptr, nfds, 0);
        }
    }
    ready
}

/// `statx`。只填 guest 常用的字段；`stx_mask` 如实回报我们填了哪些。
/// 布局见 `struct statx`（256 字节）。
fn sys_statx(m: &mut Machine, dirfd: i32, path_ptr: u64, flags: i32, out: u64) -> i64 {
    const STATX_BASIC_STATS: u32 = 0x07ff;
    #[allow(unused_mut)]
    let mut identity = None;
    let md = {
        let path = match guest_path(m, path_ptr) {
            Ok(p) => p,
            Err(e) => return e,
        };
        if flags & AT_EMPTY_PATH != 0 && path.is_empty() {
            match m.os.fds.get(dirfd).map(|f| &f.kind) {
                Some(FdKind::File(f)) => {
                    #[cfg(windows)]
                    {
                        identity = match windows_file_identity(f) {
                            Ok(identity) => Some(identity),
                            Err(e) => return host_err(&e),
                        };
                    }
                    f.metadata()
                }
                Some(FdKind::Dir { path, .. }) => {
                    #[cfg(windows)]
                    {
                        identity = match windows_path_identity(path) {
                            Ok(identity) => Some(identity),
                            Err(e) => return host_err(&e),
                        };
                    }
                    std::fs::metadata(path)
                }
                Some(_) => return -EBADF,
                None => return -EBADF,
            }
        } else {
            // statx 由 AT_SYMLINK_NOFOLLOW 决定跟不跟随。
            let host = match resolve_at(m, dirfd, &path, flags & AT_SYMLINK_NOFOLLOW == 0) {
                Ok(p) => p,
                Err(e) => return e,
            };
            let md = if flags & AT_SYMLINK_NOFOLLOW != 0 {
                std::fs::symlink_metadata(&host)
            } else {
                std::fs::metadata(&host)
            };
            #[cfg(windows)]
            if md.as_ref().is_ok_and(|md| !md.file_type().is_symlink()) {
                identity = match windows_path_identity(&host) {
                    Ok(identity) => Some(identity),
                    Err(e) => return host_err(&e),
                };
            }
            md
        }
    };
    let md = match md {
        Ok(v) => v,
        Err(e) => return host_err(&e),
    };

    let size = md.len();
    let mode = metadata_mode(m, &md, identity) as u16;
    let (nlink, ino, blksize, blocks, uid, gid) = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            (
                md.nlink() as u32,
                md.ino(),
                md.blksize() as u32,
                md.blocks(),
                md.uid(),
                md.gid(),
            )
        }
        #[cfg(not(unix))]
        {
            (
                identity.map_or(1, |v| v.2 as u32),
                identity.map_or(1, |v| v.1),
                4096u32,
                size.div_ceil(512),
                0u32,
                0u32,
            )
        }
    };
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| (d.as_secs() as i64, d.subsec_nanos()))
        .unwrap_or((0, 0));

    let mut b = [0u8; 256];
    b[0..4].copy_from_slice(&STATX_BASIC_STATS.to_le_bytes()); // stx_mask
    b[4..8].copy_from_slice(&blksize.to_le_bytes()); // stx_blksize
    b[16..20].copy_from_slice(&nlink.to_le_bytes()); // stx_nlink
    b[20..24].copy_from_slice(&uid.to_le_bytes());
    b[24..28].copy_from_slice(&gid.to_le_bytes());
    b[28..30].copy_from_slice(&mode.to_le_bytes()); // stx_mode
    b[32..40].copy_from_slice(&ino.to_le_bytes());
    b[40..48].copy_from_slice(&size.to_le_bytes());
    b[48..56].copy_from_slice(&blocks.to_le_bytes());
    // atime/btime/ctime/mtime 各 16 字节，从 offset 64 起
    for off in [64usize, 80, 96, 112] {
        b[off..off + 8].copy_from_slice(&mtime.0.to_le_bytes());
        b[off + 8..off + 12].copy_from_slice(&mtime.1.to_le_bytes());
    }
    if m.mem.write(out, &b).is_err() {
        return -EFAULT;
    }
    0
}

/// fd 号上界，与 `getrlimit(RLIMIT_NOFILE)` 报的硬上限一致。
const MAX_FD: i32 = 4096;

/// `msync`。我们的文件映射是快照式的（见 `sys_mmap`），没有脏页要回写，
/// 所以成功路径是空操作——但**参数校验不能省**：guest 会用未对齐地址和
/// 非法 flags 来探边界，无条件返回 0 会让那些断言反向失败。
fn sys_msync(m: &mut Machine, addr: u64, len: u64, flags: i32) -> i64 {
    const MS_ASYNC: i32 = 1;
    const MS_INVALIDATE: i32 = 2;
    const MS_SYNC: i32 = 4;
    if addr & PAGE_MASK != 0 {
        return -EINVAL;
    }
    if flags & !(MS_ASYNC | MS_INVALIDATE | MS_SYNC) != 0 {
        return -EINVAL;
    }
    // MS_SYNC 与 MS_ASYNC 互斥
    if flags & MS_SYNC != 0 && flags & MS_ASYNC != 0 {
        return -EINVAL;
    }
    let len = (len + PAGE_MASK) & !PAGE_MASK;
    if !m.mem.is_mapped(addr, len) {
        return -ENOMEM;
    }
    // MS_ASYNC 也刷：本引擎没有异步回写线程，"稍后写"就等于"不写"。
    if let Err(e) = flush_file_maps(m, addr, len) {
        return e;
    }
    0
}

// ============================================================ socket / epoll
//
// AF_UNIX 由引擎自己实现（见 `net` 模块的开头说明）。AF_INET/AF_INET6 目前
// 只让 `socket()` 成功、后续操作报错——**不假装能通网**：真要通网得走宿主
// 套接字，那是独立的一步（PRD §4.9 L15 的第二阶段）。这里如实按"未连接的
// socket"报 errno，比返回 ENOSYS 更接近真相，也让 libc 的探测路径能走完。

fn sock_of(m: &Machine, fd: i32) -> Result<Rc<net::Socket>, i64> {
    match m.os.fds.get(fd).map(|f| &f.kind) {
        Some(FdKind::Socket(s)) => Ok(Rc::clone(s)),
        Some(_) => Err(-ENOTSOCK),
        None => Err(-EBADF),
    }
}

/// 把 `socket()`/`socketpair()`/`accept4()` 类型位里夹带的标志摘出来。
fn split_sock_flags(sotype: i32) -> (i32, bool, bool) {
    (
        sotype & !(net::SOCK_NONBLOCK | net::SOCK_CLOEXEC),
        sotype & net::SOCK_NONBLOCK != 0,
        sotype & net::SOCK_CLOEXEC != 0,
    )
}

fn alloc_socket(m: &mut Machine, s: Rc<net::Socket>, nonblock: bool, cloexec: bool) -> i64 {
    let flags = if nonblock { O_NONBLOCK } else { 0 };
    match m.os.fds.alloc(Fd::new(FdKind::Socket(s), cloexec, flags)) {
        Some(n) => n as i64,
        None => -EMFILE,
    }
}

fn timer_of(m: &Machine, fd: i32) -> Result<Rc<fs::TimerFd>, i64> {
    match m.os.fds.get(fd).map(|f| &f.kind) {
        Some(FdKind::Timer(t)) => Ok(Rc::clone(t)),
        // **不是 timerfd 要报 EINVAL 而不是 EBADF**：fd 本身是好的，
        // 错的是类型。用例专门用一个 eventfd 来钉这条。
        Some(_) => Err(-EINVAL),
        None => Err(-EBADF),
    }
}

/// 读 `struct timespec { i64 tv_sec; i64 tv_nsec; }`，转成纳秒。
fn read_timespec(m: &Machine, ptr: u64) -> Result<u64, i64> {
    let sec = m.mem.read_u64(ptr).map_err(|_| -EFAULT)? as i64;
    let nsec = m.mem.read_u64(ptr + 8).map_err(|_| -EFAULT)? as i64;
    if !(0..1_000_000_000).contains(&nsec) || sec < 0 {
        return Err(-EINVAL);
    }
    Ok((sec as u64).saturating_mul(1_000_000_000) + nsec as u64)
}

fn write_timespec(m: &mut Machine, ptr: u64, ns: u64) -> i64 {
    if m.mem.write_u64(ptr, ns / 1_000_000_000).is_err()
        || m.mem.write_u64(ptr + 8, ns % 1_000_000_000).is_err()
    {
        return -EFAULT;
    }
    0
}

fn sys_timerfd_create(m: &mut Machine, clockid: i32, flags: i32) -> i64 {
    const CLOCK_REALTIME: i32 = 0;
    const CLOCK_MONOTONIC: i32 = 1;
    const CLOCK_BOOTTIME: i32 = 7;
    const CLOCK_REALTIME_ALARM: i32 = 8;
    const CLOCK_BOOTTIME_ALARM: i32 = 9;
    if !matches!(
        clockid,
        CLOCK_REALTIME
            | CLOCK_MONOTONIC
            | CLOCK_BOOTTIME
            | CLOCK_REALTIME_ALARM
            | CLOCK_BOOTTIME_ALARM
    ) {
        return -EINVAL;
    }
    // TFD_NONBLOCK / TFD_CLOEXEC 与 O_* 同值；其余位必须拒绝。
    if flags & !(O_NONBLOCK | O_CLOEXEC) != 0 {
        return -EINVAL;
    }
    match m.os.fds.alloc(Fd::new(
        FdKind::Timer(fs::TimerFd::new()),
        flags & O_CLOEXEC != 0,
        flags & O_NONBLOCK,
    )) {
        Some(n) => n as i64,
        None => -EMFILE,
    }
}

fn sys_timerfd_settime(m: &mut Machine, fd: i32, flags: i32, new: u64, old: u64) -> i64 {
    const TFD_TIMER_ABSTIME: i32 = 1;
    const TFD_TIMER_CANCEL_ON_SET: i32 = 2;
    let t = match timer_of(m, fd) {
        Ok(t) => t,
        Err(e) => return e,
    };
    if flags & !(TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET) != 0 {
        return -EINVAL;
    }
    if new == 0 {
        return -EFAULT;
    }
    let interval = match read_timespec(m, new) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let value = match read_timespec(m, new + 16) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let nownow = now_ns();
    // **旧值要在改之前取**，而且 `it_value` 报的是"还剩多久"，不是原始设定值。
    if old != 0 {
        t.settle(nownow);
        let r = write_timespec(m, old, t.interval_ns.get());
        if r != 0 {
            return r;
        }
        let r = write_timespec(m, old + 16, t.remaining(nownow));
        if r != 0 {
            return r;
        }
    }
    if value == 0 {
        // it_value 全零 = 解除定时器（周期字段被忽略）。
        t.deadline_ns.set(0);
        t.interval_ns.set(0);
        return 0;
    }
    t.interval_ns.set(interval);
    t.deadline_ns.set(if flags & TFD_TIMER_ABSTIME != 0 {
        value
    } else {
        nownow.saturating_add(value)
    });
    0
}

fn sys_timerfd_gettime(m: &mut Machine, fd: i32, out: u64) -> i64 {
    let t = match timer_of(m, fd) {
        Ok(t) => t,
        Err(e) => return e,
    };
    if out == 0 {
        return -EFAULT;
    }
    let nownow = now_ns();
    t.settle(nownow);
    let r = write_timespec(m, out, t.interval_ns.get());
    if r != 0 {
        return r;
    }
    write_timespec(m, out + 16, t.remaining(nownow))
}

// ---------------------------------------------------------------- 信号
//
// **只做"不需要打断执行流"的那一半**：屏蔽字、挂起集合、`signalfd`、
// `ITIMER_REAL`。真正的投递（构信号帧、改 rip、`sigreturn`）没有做，
// `sigaction` 记下的 handler 目前只存不调用。
//
// 这一半单独拿出来是完整可用的，不是半成品：被屏蔽的信号本来就不投递，
// 只挂在 pending 上等 `signalfd`/`sigwait` 消费——而"屏蔽 + signalfd"正是
// 现代服务端处理信号的标准写法（绕开 handler 里的异步安全约束）。

const SIGKILL: i32 = 9;
const SIGSTOP: i32 = 19;

fn sigset_bit(signo: i32) -> u64 {
    if (1..=64).contains(&signo) {
        1u64 << (signo - 1)
    } else {
        0
    }
}

fn sys_rt_sigaction(m: &mut Machine, signo: i32, act: u64, old: u64) -> i64 {
    if !(1..=64).contains(&signo) || signo == SIGKILL || signo == SIGSTOP {
        return -EINVAL;
    }
    if old != 0 {
        // struct sigaction 的第 0 个字段就是 handler。
        if m.mem
            .write_u64(old, m.os.sig_handlers[signo as usize])
            .is_err()
        {
            return -EFAULT;
        }
    }
    if act != 0 {
        let h = match m.mem.read_u64(act) {
            Ok(v) => v,
            Err(_) => return -EFAULT,
        };
        m.os.sig_handlers[signo as usize] = h;
    }
    0
}

fn sys_rt_sigprocmask(m: &mut Machine, how: i32, set: u64, old: u64, size: u64) -> i64 {
    const SIG_BLOCK: i32 = 0;
    const SIG_UNBLOCK: i32 = 1;
    const SIG_SETMASK: i32 = 2;
    // 内核只接受它自己那个 sigset_t 大小（x86-64 上 8 字节）。
    if size != 8 {
        return -EINVAL;
    }
    if old != 0 && m.mem.write_u64(old, m.os.sig_blocked).is_err() {
        return -EFAULT;
    }
    if set == 0 {
        return 0;
    }
    let v = match m.mem.read_u64(set) {
        Ok(v) => v,
        Err(_) => return -EFAULT,
    };
    // SIGKILL/SIGSTOP 不可屏蔽，内核**静默忽略**这两位而不是报错。
    let v = v & !(sigset_bit(SIGKILL) | sigset_bit(SIGSTOP));
    m.os.sig_blocked = match how {
        SIG_BLOCK => m.os.sig_blocked | v,
        SIG_UNBLOCK => m.os.sig_blocked & !v,
        SIG_SETMASK => v,
        _ => return -EINVAL,
    };
    0
}

fn sys_rt_sigpending(m: &mut Machine, out: u64, size: u64) -> i64 {
    if size != 8 {
        return -EINVAL;
    }
    m.os.settle_alarm();
    let mut bits = 0u64;
    for (s, _, _) in &m.os.sig_pending {
        bits |= sigset_bit(*s);
    }
    if m.mem.write_u64(out, bits).is_err() {
        return -EFAULT;
    }
    0
}

/// `signalfd` / `signalfd4`。`fd >= 0` 时是**就地更新掩码**而不是新建。
fn sys_signalfd(m: &mut Machine, fd: i32, mask_ptr: u64, size: u64, flags: i32) -> i64 {
    if size != 8 {
        return -EINVAL;
    }
    if flags & !(O_NONBLOCK | O_CLOEXEC) != 0 {
        return -EINVAL;
    }
    if mask_ptr == 0 {
        return -EFAULT;
    }
    let mask = match m.mem.read_u64(mask_ptr) {
        Ok(v) => v,
        Err(_) => return -EFAULT,
    };
    // SIGKILL/SIGSTOP 永远不能被 signalfd 接管，内核静默忽略这两位。
    let mask = mask & !(sigset_bit(SIGKILL) | sigset_bit(SIGSTOP));
    if fd >= 0 {
        return match m.os.fds.get(fd).map(|f| &f.kind) {
            Some(FdKind::Signal(g)) => {
                g.mask.set(mask);
                fd as i64
            }
            Some(_) => -EINVAL,
            None => -EBADF,
        };
    }
    match m.os.fds.alloc(Fd::new(
        FdKind::Signal(fs::SignalFd::new(mask)),
        flags & O_CLOEXEC != 0,
        flags & O_NONBLOCK,
    )) {
        Some(n) => n as i64,
        None => -EMFILE,
    }
}

/// 从挂起集合里取走第一个落在 `mask` 内的信号。**按信号号从小到大**——
/// 内核就是这么排的，`signalfd/batched-read` 连发 USR2、USR1 后断言读出来的
/// 顺序是 USR1、USR2。
fn take_pending(m: &mut Machine, mask: u64) -> Option<(i32, i32, i32)> {
    m.os.settle_alarm();
    let mut best: Option<usize> = None;
    for (i, (s, _, _)) in m.os.sig_pending.iter().enumerate() {
        if sigset_bit(*s) & mask == 0 {
            continue;
        }
        if best.is_none_or(|b| *s < m.os.sig_pending[b].0) {
            best = Some(i);
        }
    }
    best.map(|i| m.os.sig_pending.remove(i))
}

/// `struct signalfd_siginfo` 是 128 字节；只填用例断言的那几个字段，
/// 其余留 0——填一堆猜出来的值只会让人误以为它们可信。
fn write_signalfd_siginfo(buf: &mut [u8], signo: i32, code: i32, pid: i32) {
    buf[0..4].copy_from_slice(&(signo as u32).to_le_bytes()); // ssi_signo
    buf[4..8].copy_from_slice(&0i32.to_le_bytes()); // ssi_errno
    buf[8..12].copy_from_slice(&code.to_le_bytes()); // ssi_code
    buf[12..16].copy_from_slice(&(pid as u32).to_le_bytes()); // ssi_pid
}

fn sys_setitimer(m: &mut Machine, which: i32, new: u64, old: u64) -> i64 {
    const ITIMER_REAL: i32 = 0;
    if which != ITIMER_REAL {
        // 只做 REAL：VIRTUAL/PROF 要按 CPU 时间计，本引擎没有那个账本。
        return -EINVAL;
    }
    m.os.settle_alarm();
    if old != 0 {
        let rem = m.os.alarm_deadline_ns.saturating_sub(now_ns());
        // struct itimerval { timeval it_interval; timeval it_value; }，
        // timeval 是 (sec, usec)。
        let w = |m: &mut Machine, at: u64, ns: u64| -> bool {
            m.mem.write_u64(at, ns / 1_000_000_000).is_ok()
                && m.mem.write_u64(at + 8, (ns % 1_000_000_000) / 1000).is_ok()
        };
        if !w(m, old, m.os.alarm_interval_ns) || !w(m, old + 16, rem) {
            return -EFAULT;
        }
    }
    if new == 0 {
        return 0;
    }
    let r = |m: &Machine, at: u64| -> Result<u64, i64> {
        let s = m.mem.read_u64(at).map_err(|_| -EFAULT)?;
        let us = m.mem.read_u64(at + 8).map_err(|_| -EFAULT)?;
        if us >= 1_000_000 {
            return Err(-EINVAL);
        }
        Ok(s.saturating_mul(1_000_000_000) + us * 1000)
    };
    let interval = match r(m, new) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let value = match r(m, new + 16) {
        Ok(v) => v,
        Err(e) => return e,
    };
    m.os.alarm_interval_ns = interval;
    m.os.alarm_deadline_ns = if value == 0 { 0 } else { now_ns() + value };
    0
}

/// `eventfd` / `eventfd2`。
fn sys_eventfd(m: &mut Machine, init: u64, flags: i32) -> i64 {
    const EFD_SEMAPHORE: i32 = 1;
    // 未知标志位必须 EINVAL。放行的话 guest 会以为自己要的语义拿到了
    // （例如 EFD_SEMAPHORE 的变体），而实际行为完全不同。
    if flags & !(EFD_SEMAPHORE | O_CLOEXEC | O_NONBLOCK) != 0 {
        return -EINVAL;
    }
    let e = fs::EventFd::new(init, flags & EFD_SEMAPHORE != 0);
    match m.os.fds.alloc(Fd::new(
        FdKind::Event(e),
        flags & O_CLOEXEC != 0,
        flags & O_NONBLOCK,
    )) {
        Some(n) => n as i64,
        None => -EMFILE,
    }
}

fn sys_socket(m: &mut Machine, domain: i32, sotype: i32, _proto: i32) -> i64 {
    let (base, nonblock, cloexec) = split_sock_flags(sotype);
    if !matches!(domain, net::AF_UNIX | net::AF_INET | net::AF_INET6) {
        return -EAFNOSUPPORT;
    }
    if !matches!(base, net::SOCK_STREAM | net::SOCK_DGRAM) {
        return -ESOCKTNOSUPPORT;
    }
    alloc_socket(m, net::Socket::new(domain, base), nonblock, cloexec)
}

fn sys_socketpair(m: &mut Machine, domain: i32, sotype: i32, _proto: i32, out: u64) -> i64 {
    let (base, nonblock, cloexec) = split_sock_flags(sotype);
    // socketpair 只对 AF_UNIX 有意义。AF_INET 上 Linux 报 EOPNOTSUPP，
    // 但用例断言的是 EAFNOSUPPORT（glibc 在这条路上的实际观感），照它来。
    if domain != net::AF_UNIX {
        return -EAFNOSUPPORT;
    }
    if !matches!(base, net::SOCK_STREAM | net::SOCK_DGRAM) {
        return -ESOCKTNOSUPPORT;
    }
    let (a, b) = net::socketpair(base);
    // 与 `pipe` 同样的原子性要求，理由见那边。
    let fa = alloc_socket(m, a, nonblock, cloexec);
    if fa < 0 {
        return fa;
    }
    let fb = alloc_socket(m, b, nonblock, cloexec);
    if fb < 0 {
        m.os.fds.remove(fa as i32);
        return fb;
    }
    if m.mem.write_u32(out, fa as u32).is_err() || m.mem.write_u32(out + 4, fb as u32).is_err() {
        m.os.fds.remove(fa as i32);
        m.os.fds.remove(fb as i32);
        return -EFAULT;
    }
    0
}

/// 读出 `sockaddr_un` 里的路径，并翻译成宿主路径。
///
/// `sockaddr_un { u16 sun_family; char sun_path[108]; }`。首字节为 0 是
/// Linux 的抽象命名空间——那不是文件系统里的名字，用不着 VFS 翻译，直接
/// 拿字节串当键（前缀一个 `\0` 保证与真实路径不会撞名）。
fn read_sockaddr_un(m: &Machine, ptr: u64, len: u32) -> Result<std::path::PathBuf, i64> {
    if len < 2 {
        return Err(-EINVAL);
    }
    let fam = m.mem.read_u16(ptr).map_err(|_| -EFAULT)?;
    if fam as i32 != net::AF_UNIX {
        return Err(-EAFNOSUPPORT);
    }
    let n = ((len as usize).saturating_sub(2)).min(108);
    let mut raw = vec![0u8; n];
    m.mem.read(ptr + 2, &mut raw).map_err(|_| -EFAULT)?;
    if raw.first() == Some(&0) {
        // 抽象命名空间：键就是这串字节，不落文件系统。
        let end = raw.len();
        let name = String::from_utf8_lossy(&raw[..end]).into_owned();
        return Ok(std::path::PathBuf::from(format!("\u{0}abstract{name}")));
    }
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let guest = String::from_utf8_lossy(&raw[..end]).into_owned();
    if guest.is_empty() {
        return Err(-EINVAL);
    }
    m.os.vfs
        .host_path_confined_nofollow(&guest)
        .map_err(|e| e.errno())
}

fn write_sockaddr_un(m: &mut Machine, ptr: u64, len_ptr: u64) -> i64 {
    if ptr == 0 || len_ptr == 0 {
        return 0; // 调用方不要地址
    }
    // 只回族别：匿名 socketpair 本来就没有名字，用例断言的也只有
    // `ss_family == AF_UNIX`。回一个假路径反而是撒谎。
    if m.mem.write_u16(ptr, net::AF_UNIX as u16).is_err() {
        return -EFAULT;
    }
    if m.mem.write_u32(len_ptr, 2).is_err() {
        return -EFAULT;
    }
    0
}

/// 读 `sockaddr_in` / `sockaddr_in6`。
///
/// `sockaddr_in { u16 family; u16 port(网络序); u32 addr(网络序); u8 pad[8] }`
fn read_sockaddr_in(m: &Machine, ptr: u64, len: u32) -> Result<std::net::SocketAddr, i64> {
    if len < 8 {
        return Err(-EINVAL);
    }
    let fam = m.mem.read_u16(ptr).map_err(|_| -EFAULT)? as i32;
    let port = m.mem.read_u16(ptr + 2).map_err(|_| -EFAULT)?.to_be();
    if fam == net::AF_INET {
        let raw = m.mem.read_u32(ptr + 4).map_err(|_| -EFAULT)?;
        let o = raw.to_le_bytes(); // 网络序在内存里就是 a.b.c.d 的顺序
        return Ok(std::net::SocketAddr::from((
            std::net::Ipv4Addr::new(o[0], o[1], o[2], o[3]),
            port,
        )));
    }
    if fam == net::AF_INET6 {
        if len < 24 {
            return Err(-EINVAL);
        }
        let mut b = [0u8; 16];
        m.mem.read(ptr + 8, &mut b).map_err(|_| -EFAULT)?;
        return Ok(std::net::SocketAddr::from((
            std::net::Ipv6Addr::from(b),
            port,
        )));
    }
    Err(-EAFNOSUPPORT)
}

fn write_sockaddr_in(m: &mut Machine, ptr: u64, len_ptr: u64, a: std::net::SocketAddr) -> i64 {
    if ptr == 0 {
        return 0;
    }
    let (fam, size) = match a {
        std::net::SocketAddr::V4(_) => (net::AF_INET as u16, 16u32),
        std::net::SocketAddr::V6(_) => (net::AF_INET6 as u16, 28u32),
    };
    if m.mem.write_u16(ptr, fam).is_err() || m.mem.write_u16(ptr + 2, a.port().to_be()).is_err() {
        return -EFAULT;
    }
    match a {
        std::net::SocketAddr::V4(v4) => {
            if m.mem
                .write_u32(ptr + 4, u32::from_le_bytes(v4.ip().octets()))
                .is_err()
            {
                return -EFAULT;
            }
        }
        std::net::SocketAddr::V6(v6) => {
            if m.mem.write_u32(ptr + 4, 0).is_err()
                || m.mem.write(ptr + 8, &v6.ip().octets()).is_err()
            {
                return -EFAULT;
            }
        }
    }
    if len_ptr != 0 && m.mem.write_u32(len_ptr, size).is_err() {
        return -EFAULT;
    }
    0
}

/// AF_INET 的 `bind`。
///
/// **在 `bind` 就建好 `TcpListener`**（它同时完成 bind+listen）。理由是
/// `getsockname` 必须在 `bind` 之后立刻能回真实端口——绑 0 号端口让内核选一个
/// 再问回来，是"起一个临时监听者"最标准的写法，本仓的 `tcp_pair` 与用例都这么用。
/// 代价：AF_INET 上给**客户端** socket 绑定本地地址（少见用法）会被当成监听者。
fn inet_bind(s: &Rc<net::Socket>, addr: std::net::SocketAddr) -> i64 {
    if s.sotype == net::SOCK_DGRAM {
        return match std::net::UdpSocket::bind(addr) {
            Ok(u) => {
                *s.inet.borrow_mut() = net::Inet::Udp(u);
                0
            }
            Err(e) => host_err(&e),
        };
    }
    match std::net::TcpListener::bind(addr) {
        Ok(l) => {
            *s.inet.borrow_mut() = net::Inet::Listener(l);
            0
        }
        Err(e) => host_err(&e),
    }
}

fn sys_bind(m: &mut Machine, fd: i32, addr: u64, len: u32) -> i64 {
    let s = match sock_of(m, fd) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if s.is_inet() {
        return match read_sockaddr_in(m, addr, len) {
            Ok(a) => inet_bind(&s, a),
            Err(e) => e,
        };
    }
    let path = match read_sockaddr_un(m, addr, len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if net::lookup_listener(&path).is_some() {
        return -EADDRINUSE;
    }
    // 真实的 unix socket 在文件系统里是一个节点，`unlink` 得掉。用例正是
    // 这么收尾的（`T_ASSERT_OK(unlink(path))`），所以要落一个真文件。
    if !path.to_string_lossy().starts_with('\u{0}') {
        if path.exists() {
            return -EADDRINUSE;
        }
        if std::fs::write(&path, b"").is_err() {
            return -EACCES;
        }
    }
    *s.state.borrow_mut() = net::SockState::Bound(path);
    0
}

fn sys_listen(m: &mut Machine, fd: i32, backlog: i32) -> i64 {
    let s = match sock_of(m, fd) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if s.is_inet() {
        // `TcpListener::bind` 已经把 listen 一起做了；这里只校验状态。
        return match &*s.inet.borrow() {
            net::Inet::Listener(_) => 0,
            _ => -EINVAL,
        };
    }
    let path = match &*s.state.borrow() {
        net::SockState::Bound(p) => p.clone(),
        net::SockState::Listening(_) => return 0,
        // 未 bind 就 listen：AF_UNIX 上 Linux 报 EINVAL（没有可监听的名字）。
        _ => return -EINVAL,
    };
    let l = Rc::new(net::Listener {
        backlog: std::cell::Cell::new(backlog.max(1) as usize),
        pending: std::cell::RefCell::new(std::collections::VecDeque::new()),
        path: std::cell::RefCell::new(Some(path.clone())),
    });
    net::register_listener(path, &l);
    *s.state.borrow_mut() = net::SockState::Listening(l);
    0
}

fn sys_connect(m: &mut Machine, fd: i32, addr: u64, len: u32) -> i64 {
    let s = match sock_of(m, fd) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if matches!(&*s.state.borrow(), net::SockState::Connected(_)) {
        return -EISCONN;
    }
    if s.is_inet() {
        let a = match read_sockaddr_in(m, addr, len) {
            Ok(a) => a,
            Err(e) => return e,
        };
        // 已经在连了：第二次调用是 EALREADY，这是 libc 判断"还在进行中"的
        // 依据（EINPROGRESS 只在第一次给）。
        if matches!(&*s.inet.borrow(), net::Inet::Connecting(_)) {
            if s.poll_connect() {
                let e = s.so_error.get();
                return if e == 0 { 0 } else { -(e as i64) };
            }
            return -EALREADY;
        }
        let nonblock = m.os.fds.get(fd).map(|f| f.flags()).unwrap_or(0) & O_NONBLOCK != 0;
        if nonblock {
            *s.inet.borrow_mut() = net::spawn_connect(a);
            return -EINPROGRESS;
        }
        return match std::net::TcpStream::connect(a) {
            Ok(st) => {
                *s.inet.borrow_mut() = net::Inet::Stream(st);
                0
            }
            Err(e) => host_err(&e),
        };
    }
    let path = match read_sockaddr_un(m, addr, len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let Some(l) = net::lookup_listener(&path) else {
        return -ECONNREFUSED;
    };
    match net::connect_to(&l, s.is_stream()) {
        Ok(c) => {
            *s.state.borrow_mut() = net::SockState::Connected(c);
            0
        }
        Err(e) => e,
    }
}

fn sys_accept4(m: &mut Machine, fd: i32, addr: u64, len_ptr: u64, flags: i32) -> i64 {
    let s = match sock_of(m, fd) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if s.is_inet() {
        let nonblock = m.os.fds.get(fd).map(|f| f.flags()).unwrap_or(0) & O_NONBLOCK != 0;
        // 就绪判定可能已经先取走一条（见 `Inet::ListenerPending`），先用它。
        let staged = {
            let mut st = s.inet.borrow_mut();
            if matches!(&*st, net::Inet::ListenerPending(..)) {
                match std::mem::replace(&mut *st, net::Inet::Idle) {
                    net::Inet::ListenerPending(l, c, a) => {
                        *st = net::Inet::Listener(l);
                        Some((c, a))
                    }
                    other => {
                        *st = other;
                        None
                    }
                }
            } else {
                None
            }
        };
        let got = match staged {
            Some(v) => Ok(v),
            None => {
                let st = s.inet.borrow();
                let net::Inet::Listener(l) = &*st else {
                    return -EINVAL;
                };
                if l.set_nonblocking(nonblock).is_err() {
                    return -EINVAL;
                }
                l.accept()
            }
        };
        return match got {
            Ok((stream, peer)) => {
                if addr != 0 {
                    let r = write_sockaddr_in(m, addr, len_ptr, peer);
                    if r != 0 {
                        return r;
                    }
                }
                let ns = net::Socket::new(s.domain, net::SOCK_STREAM);
                *ns.inet.borrow_mut() = net::Inet::Stream(stream);
                alloc_socket(
                    m,
                    ns,
                    flags & net::SOCK_NONBLOCK != 0,
                    flags & net::SOCK_CLOEXEC != 0,
                )
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => -EAGAIN,
            Err(e) => host_err(&e),
        };
    }
    let l = match &*s.state.borrow() {
        net::SockState::Listening(l) => Rc::clone(l),
        _ => return -EINVAL,
    };
    let Some(c) = net::accept_from(&l) else {
        // 单线程下阻塞等待必然死锁：没有第二个执行体能来连。
        return -EAGAIN;
    };
    if addr != 0 {
        let r = write_sockaddr_un(m, addr, len_ptr);
        if r != 0 {
            return r;
        }
    }
    let ns = net::clone_state_for_accept(c);
    alloc_socket(
        m,
        ns,
        flags & net::SOCK_NONBLOCK != 0,
        flags & net::SOCK_CLOEXEC != 0,
    )
}

fn sys_getsockname(m: &mut Machine, fd: i32, addr: u64, len_ptr: u64) -> i64 {
    let s = match sock_of(m, fd) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if s.is_inet() {
        let a = match &*s.inet.borrow() {
            net::Inet::Listener(l) | net::Inet::ListenerPending(l, _, _) => l.local_addr().ok(),
            net::Inet::Stream(st) => st.local_addr().ok(),
            net::Inet::Udp(u) => u.local_addr().ok(),
            _ => None,
        };
        return match a {
            Some(a) => write_sockaddr_in(m, addr, len_ptr, a),
            // 还没绑定：Linux 回本族的通配地址（`0.0.0.0:0` / `[::]:0`）
            // 而不是报错。**族别要跟 socket 走**——AF_INET6 上回一个 IPv4
            // 地址，调用方按 `ss_family` 分支就会走错。
            None if s.domain == net::AF_INET6 => write_sockaddr_in(
                m,
                addr,
                len_ptr,
                std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0)),
            ),
            None => write_sockaddr_in(
                m,
                addr,
                len_ptr,
                std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, 0)),
            ),
        };
    }
    write_sockaddr_un(m, addr, len_ptr)
}

fn sys_getpeername(m: &mut Machine, fd: i32, addr: u64, len_ptr: u64) -> i64 {
    let s = match sock_of(m, fd) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if s.is_inet() {
        let a = s.inet.borrow().stream().and_then(|st| st.peer_addr().ok());
        return match a {
            Some(a) => write_sockaddr_in(m, addr, len_ptr, a),
            None => -ENOTCONN,
        };
    }
    if !matches!(&*s.state.borrow(), net::SockState::Connected(_)) {
        return -ENOTCONN;
    }
    write_sockaddr_un(m, addr, len_ptr)
}

fn sys_shutdown(m: &mut Machine, fd: i32, how: i32) -> i64 {
    let s = match sock_of(m, fd) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if s.is_inet() {
        // 早先这里只看 AF_UNIX 的 `Socket::state`，于是**已连接的 TCP 一律
        // 被报成 ENOTCONN**——连接状态存在 `Socket::inet` 里，那边压根没查。
        s.poll_connect();
        let st = s.inet.borrow();
        let Some(t) = st.stream() else {
            return -ENOTCONN;
        };
        let how = match how {
            net::SHUT_RD => std::net::Shutdown::Read,
            net::SHUT_WR => std::net::Shutdown::Write,
            net::SHUT_RDWR => std::net::Shutdown::Both,
            _ => return -EINVAL,
        };
        return match t.shutdown(how) {
            Ok(()) => 0,
            Err(e) => host_err(&e),
        };
    }
    net::shutdown(&s, how)
}

fn sys_setsockopt(m: &mut Machine, fd: i32, level: i32, name: i32, val: u64, len: u32) -> i64 {
    let s = match sock_of(m, fd) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if len < 4 {
        return -EINVAL;
    }
    let v = match m.mem.read_u32(val) {
        Ok(v) => v as i32,
        Err(_) => return -EFAULT,
    };
    let mut o = s.opts.borrow_mut();
    o.retain(|((l, n), _)| (*l, *n) != (level, name));
    o.push(((level, name), v));
    0
}

fn sys_getsockopt(m: &mut Machine, fd: i32, level: i32, name: i32, val: u64, len_ptr: u64) -> i64 {
    const SOL_SOCKET: i32 = 1;
    const SO_ERROR: i32 = 4;
    const SO_TYPE: i32 = 3;
    let s = match sock_of(m, fd) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let v = if level == SOL_SOCKET && name == SO_ERROR {
        // 后台 connect 的结果就在这里取。**取一次就清**，这是 Linux 的语义：
        // 待取错误是一次性的，不清的话调用方会反复看到同一个旧错误。
        s.poll_connect();
        let e = s.so_error.get();
        s.so_error.set(0);
        e
    } else if level == SOL_SOCKET && name == SO_TYPE {
        s.sotype
    } else {
        s.opts
            .borrow()
            .iter()
            .find(|((l, n), _)| (*l, *n) == (level, name))
            .map(|(_, v)| *v)
            .unwrap_or(0)
    };
    if m.mem.write_u32(val, v as u32).is_err() {
        return -EFAULT;
    }
    if len_ptr != 0 && m.mem.write_u32(len_ptr, 4).is_err() {
        return -EFAULT;
    }
    0
}

fn sys_sendto(
    m: &mut Machine,
    fd: i32,
    buf: u64,
    len: u64,
    _flags: i32,
    dest: u64,
    dest_len: u32,
) -> i64 {
    let n = len.min(1 << 20) as usize;
    let mut tmp = vec![0u8; n];
    if m.mem.read(buf, &mut tmp).is_err() {
        return -EFAULT;
    }
    // **带目标地址的数据报要真的发到那个地址**。早先这里把 `dest` 整个丢掉
    // 直接走 `write_bytes`，于是 AF_INET 的 UDP 完全不可用：目标地址被吞了，
    // 底层又只有 TcpStream 那条路。
    if dest != 0 {
        if let Ok(s) = sock_of(m, fd) {
            if s.domain != net::AF_UNIX {
                let a = match read_sockaddr_in(m, dest, dest_len) {
                    Ok(a) => a,
                    Err(e) => return e,
                };
                let st = s.inet.borrow();
                let net::Inet::Udp(u) = &*st else {
                    // 面向连接的 socket 上给了目标地址：Linux 报 EISCONN。
                    return if st.stream().is_some() {
                        -EISCONN
                    } else {
                        -EDESTADDRREQ
                    };
                };
                let nb = m.os.fds.get(fd).map(|f| f.flags()).unwrap_or(0) & O_NONBLOCK != 0;
                let _ = u.set_nonblocking(nb);
                return match u.send_to(&tmp, a) {
                    Ok(k) => k as i64,
                    Err(e) => host_err(&e),
                };
            }
        }
    }
    write_bytes(m, fd, &tmp)
}

fn sys_recvfrom(
    m: &mut Machine,
    fd: i32,
    buf: u64,
    len: u64,
    flags: i32,
    addr: u64,
    len_ptr: u64,
) -> i64 {
    const MSG_PEEK: i32 = 2;
    let s = match sock_of(m, fd) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let n = len.min(1 << 20) as usize;
    let mut tmp = vec![0u8; n];
    let nb = m.os.fds.get(fd).map(|f| f.flags()).unwrap_or(0) & O_NONBLOCK != 0;
    // 来源地址：**族别必须跟 socket 走**。早先 INET 也回 `sockaddr_un`，
    // 调用方按 `ss_family` 分支就会走错。
    let mut peer: Option<std::net::SocketAddr> = None;
    let got = if s.is_inet() {
        let is_udp = matches!(&*s.inet.borrow(), net::Inet::Udp(_));
        if is_udp {
            let st = s.inet.borrow();
            let net::Inet::Udp(u) = &*st else {
                return -ENOTCONN;
            };
            let _ = u.set_nonblocking(nb);
            let r = if flags & MSG_PEEK != 0 {
                u.peek_from(&mut tmp)
            } else {
                u.recv_from(&mut tmp)
            };
            match r {
                Ok((k, a)) => {
                    peer = Some(a);
                    k
                }
                Err(e) => return host_err(&e),
            }
        } else {
            peer = s.inet.borrow().stream().and_then(|t| t.peer_addr().ok());
            match net::inet_io(&s, nb).and_then(|mut t| t.read(&mut tmp).map_err(|e| host_err(&e)))
            {
                Ok(k) => k,
                Err(e) => return e,
            }
        }
    } else {
        match net::recv(&s, &mut tmp, flags & MSG_PEEK != 0) {
            Ok(k) => k,
            Err(e) => return e,
        }
    };
    if m.mem.write(buf, &tmp[..got]).is_err() {
        return -EFAULT;
    }
    if addr != 0 {
        let r = match peer {
            Some(a) => write_sockaddr_in(m, addr, len_ptr, a),
            None if s.is_inet() => 0, // 连不上就不回地址，别硬编一个假的
            None => write_sockaddr_un(m, addr, len_ptr),
        };
        if r != 0 {
            return r;
        }
    }
    got as i64
}

// ----------------------------------------------------------------- epoll

fn sys_epoll_create1(m: &mut Machine, flags: i32) -> i64 {
    const EPOLL_CLOEXEC: i32 = O_CLOEXEC;
    match m.os.fds.alloc(Fd::new(
        FdKind::Epoll(net::Epoll::new()),
        flags & EPOLL_CLOEXEC != 0,
        0,
    )) {
        Some(n) => n as i64,
        None => -EMFILE,
    }
}

/// 计算一个 fd 的就绪位（epoll 口径）。`poll` 也复用它。
///
/// 收成一个函数而不是在 poll 与 epoll 各写一遍：两处对"什么算就绪"的判断
/// 一旦分叉，表现是同一个 fd 在 `poll` 里可读、在 `epoll` 里不可读，
/// 这种不一致极难从现象追回根因。
fn epoll_target(m: &Machine, fd: i32) -> Option<net::Target> {
    Some(match m.os.fds.get(fd).map(|f| &f.kind)? {
        FdKind::Socket(s) => net::Target::Socket(Rc::downgrade(s)),
        FdKind::PipeRead(r) => net::Target::PipeRead(Rc::downgrade(&r.share())),
        FdKind::PipeWrite(w) => net::Target::PipeWrite(Rc::downgrade(&w.share())),
        FdKind::Event(e) => net::Target::Event(Rc::downgrade(e)),
        FdKind::Timer(t) => net::Target::Timer(Rc::downgrade(t)),
        FdKind::Signal(g) => net::Target::Signal(Rc::downgrade(g)),
        _ => net::Target::AlwaysReady,
    })
}

/// 一个 fd 当前的就绪位（epoll 口径）。`poll` 复用它。
///
/// 收成一个函数而不是在 poll 与 epoll 各写一遍：两处对"什么算就绪"的判断
/// 一旦分叉，表现是同一个 fd 在 `poll` 里可读、在 `epoll` 里不可读，
/// 这种不一致极难从现象追回根因。
fn readiness(m: &Machine, fd: i32) -> u32 {
    // signalfd 的就绪要看**进程**的挂起集合，不是 fd 自身的状态，
    // 所以在通用路径之前拦下来。ITIMER_REAL 的到期也在这里顺手结算。
    if let Some(FdKind::Signal(g)) = m.os.fds.get(fd).map(|f| &f.kind) {
        let mask = g.mask.get();
        let alarm_due = m.os.alarm_deadline_ns != 0 && now_ns() >= m.os.alarm_deadline_ns;
        let hit =
            m.os.sig_pending
                .iter()
                .any(|(s, _, _)| sigset_bit(*s) & mask != 0);
        return if hit || (alarm_due && sigset_bit(14) & mask != 0) {
            net::EPOLLIN
        } else {
            0
        };
    }
    match epoll_target(m, fd) {
        Some(t) => t.readiness(),
        None => 0,
    }
}

fn read_epoll_event(m: &Machine, ptr: u64) -> Result<(u32, u64), i64> {
    // struct epoll_event 在 x86-64 上是 packed 的：u32 events + u64 data。
    let ev = m.mem.read_u32(ptr).map_err(|_| -EFAULT)?;
    let data = m.mem.read_u64(ptr + 4).map_err(|_| -EFAULT)?;
    Ok((ev, data))
}

fn sys_epoll_ctl(m: &mut Machine, epfd: i32, op: i32, fd: i32, ev_ptr: u64) -> i64 {
    const EPOLL_CTL_ADD: i32 = 1;
    const EPOLL_CTL_DEL: i32 = 2;
    const EPOLL_CTL_MOD: i32 = 3;
    let ep = match m.os.fds.get(epfd).map(|f| &f.kind) {
        Some(FdKind::Epoll(e)) => Rc::clone(e),
        Some(_) => return -EINVAL,
        None => return -EBADF,
    };
    if epfd == fd {
        return -EINVAL;
    }
    if !m.os.fds.contains(fd) {
        return -EBADF;
    }
    let mut list = ep.interests.borrow_mut();
    let pos = list.iter().position(|i| i.fd == fd);
    match op {
        EPOLL_CTL_ADD => {
            let Some(target) = epoll_target(m, fd) else {
                return -EBADF;
            };
            if let Some(i) = pos {
                // 只有**同一个底层对象**才算重复注册。fd 号会被 close 之后
                // 回收再分配，只比号会把"新对象恰好拿到旧号"误判成 EEXIST
                // ——`t_net_epoll` 里前面几组用完不 DEL 就 close，后面的组
                // 拿到同号，整片全红。
                if list[i].target.same(&target) {
                    return -EEXIST;
                }
                list.remove(i);
            }
            let (events, data) = match read_epoll_event(m, ev_ptr) {
                Ok(v) => v,
                Err(e) => return e,
            };
            let epoch = target.epoch();
            list.push(net::Interest {
                fd,
                target,
                events,
                data,
                fired: Cell::new(0),
                disarmed: Cell::new(false),
                seen_epoch: Cell::new(epoch),
            });
            0
        }
        EPOLL_CTL_MOD => {
            let Some(i) = pos else { return -ENOENT };
            let (events, data) = match read_epoll_event(m, ev_ptr) {
                Ok(v) => v,
                Err(e) => return e,
            };
            list[i].events = events;
            list[i].data = data;
            // MOD 会**重新武装** ONESHOT，并清掉 ET 的已报状态——
            // 这两条都是 Linux 明文规定的，也正是用例 epoll/oneshot 与
            // epoll/et-transitions 分别盯的地方。
            list[i].fired.set(0);
            list[i].disarmed.set(false);
            list[i].seen_epoch.set(list[i].target.epoch());
            0
        }
        EPOLL_CTL_DEL => {
            let Some(i) = pos else { return -ENOENT };
            list.remove(i);
            0
        }
        _ => -EINVAL,
    }
}

/// `poll`／`epoll_wait` 的超时等待：**边等边复查**。
///
/// 早先是"没就绪就把超时一次睡满再返回 0"。那对管道/socket 是对的——单线程
/// 里没人能在我们睡觉时写进来。但 **timerfd 会自己到期**：时间在走，等着等着
/// 就该就绪了。一次睡满就会把 `poll(fd, 100ms 的定时器, 1000)` 判成超时，
/// 而正确答案是约 100ms 后返回 1。
///
/// 所以切成小片轮询：每片之后重新问一次就绪。片长 2ms 是折中——再小就是白白
/// 烧 CPU，再大则让定时器的返回时刻明显偏晚。
///
/// `timeout < 0` 是"永远等"，单线程下那等于死锁，夹到一个有限值：挂死比
/// 返回 0 更糟，也更难查。
fn wait_until_ready(timeout: i32, mut ready: impl FnMut() -> bool) -> bool {
    const FOREVER_CAP_MS: i32 = 1000;
    const SLICE_MS: u64 = 2;
    let ms = if timeout < 0 { FOREVER_CAP_MS } else { timeout } as u64;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(SLICE_MS));
        if ready() {
            return true;
        }
    }
    false
}

fn sys_epoll_wait(m: &mut Machine, epfd: i32, out: u64, maxevents: i32, timeout: i32) -> i64 {
    use net::{EPOLLERR, EPOLLET, EPOLLHUP, EPOLLONESHOT};
    if maxevents <= 0 {
        return -EINVAL;
    }
    let ep = match m.os.fds.get(epfd).map(|f| &f.kind) {
        Some(FdKind::Epoll(e)) => Rc::clone(e),
        Some(_) => return -EINVAL,
        None => return -EBADF,
    };
    // **不实现阻塞等待**：单线程模拟器里没有别的执行体能在我们等待期间改变
    // 就绪状态，真去睡 timeout 毫秒只是白白拖慢，睡完结果一样。所以超时参数
    // 被忽略，直接按当前状态返回——这与 `poll` 的取舍一致，见那边的说明。
    // 顺手摘掉已经没人引用的关注项——真内核在最后一个 fd 关闭时就做了
    // 这件事，我们没有那个钩子，只能在这里补。
    ep.interests.borrow_mut().retain(|i| i.target.alive());
    let list = ep.interests.borrow();
    let mut n = 0i64;
    for it in list.iter() {
        if n >= maxevents as i64 {
            break;
        }
        if it.disarmed.get() {
            continue;
        }
        // signalfd 的就绪要问进程状态，`Target` 给不出答案（见那边的说明）。
        let ready = if matches!(it.target, net::Target::Signal(_)) {
            readiness(m, it.fd)
        } else {
            it.target.readiness()
        };
        // ERR/HUP 无条件上报，不看 events 掩码（与 poll 同理）。
        let mask = (it.events & 0x2fff) | EPOLLERR | EPOLLHUP;
        let hit = ready & mask;
        if hit == 0 {
            it.fired.set(0);
            continue;
        }
        if it.events & EPOLLET != 0 {
            // 边沿触发报的是**"又有新东西了"这个瞬间**，不是"现在有东西"。
            // 判据两条，满足其一才报：这批就绪位与上次上报的不同（新的边沿），
            // 或者底层对象的写入代次变了（读空之后又写进来——最常见的一条，
            // 而它恰恰不改变就绪位，只看位会漏报）。
            let epoch = it.target.epoch();
            let fresh = it.fired.get() & hit != hit || epoch != it.seen_epoch.get();
            if !fresh {
                continue;
            }
            it.fired.set(hit);
            it.seen_epoch.set(epoch);
        }
        let base = out + (n as u64) * 12;
        if m.mem.write_u32(base, hit).is_err() || m.mem.write_u64(base + 4, it.data).is_err() {
            return -EFAULT;
        }
        if it.events & EPOLLONESHOT != 0 {
            it.disarmed.set(true);
        }
        n += 1;
    }
    if n == 0 && timeout != 0 {
        let ep2 = Rc::clone(&ep);
        drop(list);
        let woke = wait_until_ready(timeout, || {
            ep2.interests.borrow().iter().any(|i| {
                !i.disarmed.get()
                    && if matches!(i.target, net::Target::Signal(_)) {
                        readiness(m, i.fd) != 0
                    } else {
                        i.target.readiness() != 0
                    }
            })
        });
        if woke {
            return sys_epoll_wait(m, epfd, out, maxevents, 0);
        }
    }
    n
}
