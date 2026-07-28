//! Linux x86-64 syscall 模拟。
//!
//! 约定：每个 `sys_*` 返回 `i64`，负值是 `-errno`（Linux 内核的 ABI）。
//! guest 侧的 libc 会自己把负值翻成 `errno`。
//!
//! **没实现的 syscall 一律返回 `-ENOSYS` 并在 `WBOX_STRACE=1` 下打印**，
//! 绝不假装成功——假装成功会让 guest 在离故障点很远的地方以莫名其妙的方式坏掉。

pub mod fs;
pub mod process;

use crate::cpu::{R10, R11, R8, R9, RAX, RCX, RDI, RDX, RSI};
use crate::machine::{Exception, ExecResult, Machine};
use crate::mem::{PAGE_MASK, PROT_EXEC, PROT_READ, PROT_WRITE};
use fs::{Fd, FdKind, FdTable, Vfs};
use std::cell::Cell;
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
    /// `umask` 的当前值。只记录不生效，见分派表里的说明。
    pub umask: u32,
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
            pid_alloc: Rc::clone(&self.pid_alloc),
            zombies: Vec::new(),
            fork_depth: self.fork_depth + 1,
            tmpfile_seq: 0,
            exe: self.exe.clone(),
            tid_address: self.tid_address,
            strace: self.strace,
            release: self.release.clone(),
            umask: self.umask,
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
        // 信号：模拟器目前不投递信号，登记动作直接成功（guest 装了处理器
        // 但永远收不到，行为等价于"没有信号发生"）。
        13 | 14 | 131 => 0,
        16 => sys_ioctl(m, a[0] as i32, a[1], a[2]),
        17 => sys_pread(m, a[0] as i32, a[1], a[2], a[3] as i64),
        // preadv/pwritev（以及带 flags 的 v2 变体）。偏移是 pos_l|pos_h<<32，
        // 我们只支持 64 位宿主，pos_h 恒为 0，直接取 a[3]。
        295 | 327 => sys_preadv(m, a[0] as i32, a[1], a[2], a[3] as i64),
        296 | 328 => sys_pwritev(m, a[0] as i32, a[1], a[2], a[3] as i64),
        19 => sys_readv(m, a[0] as i32, a[1], a[2]),
        20 => sys_writev(m, a[0] as i32, a[1], a[2]),
        21 => sys_access(m, a[0], a[1] as i32),
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
        97 | 302 => sys_getrlimit(m, nr, &a),
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
        77 => sys_ftruncate(m, a[0] as i32, a[1] as i64),
        81 => sys_fchdir(m, a[0] as i32),
        82 => sys_rename(m, AT_FDCWD, a[0], AT_FDCWD, a[1]),
        264 | 316 => sys_rename(m, a[0] as i32, a[1], a[2] as i32, a[3]),
        83 => sys_mkdir(m, AT_FDCWD, a[0]),
        258 => sys_mkdir(m, a[0] as i32, a[1]),
        84 => sys_unlinkat(m, AT_FDCWD, a[0], AT_REMOVEDIR),
        85 => sys_openat(m, AT_FDCWD, a[0], O_WRONLY | O_CREAT | O_TRUNC, a[1] as u32),
        86 => sys_link(m, a[0], a[1]),
        265 => sys_link(m, a[1], a[3]),
        88 => sys_symlink(m, a[0], a[1]),
        266 => sys_symlink(m, a[0], a[2]),
        // chmod/chown 族：容器内一律 root，且 Windows 没有对应语义。
        // 报成功而不是 ENOSYS——guest 的 install/cp 会因为 chmod 失败而整体失败，
        // 而这里的"失败"并不代表任何真实的权限问题。
        90 | 91 | 92 | 93 | 260 | 268 => 0,
        // utimensat 族：时间戳设置暂不落到宿主，报成功。
        132 | 235 | 280 => 0,
        95 => {
            // umask：只记住值并返回旧值，不参与实际创建权限（同上）
            let old = m.os.umask;
            m.os.umask = a[0] as u32 & 0o777;
            old as i64
        }
        269 => sys_access(m, a[1], a[2] as i32),
        292 => sys_dup3(m, a[0] as i32, a[1] as i32, a[2] as i32),
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
        Some(FdKind::PipeRead(inner)) => {
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
        Some(FdKind::File(f)) => f.write(data),
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
        Some(FdKind::PipeWrite(w)) => {
            w.inner().data.borrow_mut().extend(data.iter().copied());
            Ok(data.len())
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
        Some(FdKind::PipeRead(inner)) => FdKind::PipeRead(std::rc::Rc::clone(inner)),
        Some(FdKind::PipeWrite(w)) => FdKind::PipeWrite(w.clone()),
        _ => return -EBADF,
    };
    let flags = m.os.fds.get(fd).map(|f| f.flags).unwrap_or(0);
    // dup/dup2 产生的新 fd **不继承** O_CLOEXEC，这是 POSIX 明确规定的。
    let nf = Fd {
        kind,
        cloexec: false,
        flags,
    };
    match at {
        None => m.os.fds.alloc_min(nf, min) as i64,
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
        F_GETFL => m.os.fds.get(fd).map(|f| f.flags as i64).unwrap_or(-EBADF),
        F_SETFL => {
            if let Some(f) = m.os.fds.get_mut(fd) {
                // 只有这几位是 SETFL 可改的
                let keep = f.flags & !(O_APPEND | O_NONBLOCK);
                f.flags = keep | (arg as i32 & (O_APPEND | O_NONBLOCK));
            }
            0
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
        opt.mode(mode & 0o777);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        const FILE_SHARE_WRITE: u32 = 2;
        const FILE_SHARE_DELETE: u32 = 4;
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        let _ = mode;
        opt.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE);
    }
    let f = match opt.open(&path) {
        Ok(f) => f,
        Err(e) => return host_err(&e),
    };
    #[cfg(unix)]
    if let Err(e) = std::fs::remove_file(&path) {
        // 删不掉就别把这个 fd 交出去：交出去 = 在 rootfs 里留下可见垃圾，
        // 而 guest 以为它拿到的是无名文件。
        drop(f);
        let _ = std::fs::remove_file(&path);
        return host_err(&e);
    }
    m.os.fds.alloc(Fd {
        kind: FdKind::File(f),
        cloexec: flags & O_CLOEXEC != 0,
        // O_TMPFILE 之外的 O_DIRECTORY 位不该回给 F_GETFL，摘掉。
        flags: flags & !(O_TMPFILE | O_DIRECTORY),
    }) as i64
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

    // 空路径永远是 ENOENT。不显式判的话它会被当成"当前目录"，
    // `open("", O_RDONLY)` 就会成功返回一个目录 fd。
    if path.is_empty() {
        return -ENOENT;
    }

    // 合成的 /dev/*：必须在碰宿主文件系统之前拦下来，容器 rootfs 里通常
    // 没有 /dev，Windows 宿主上更是根本没有这些路径。
    if let Some(d) = fs::DevKind::from_guest_path(&path) {
        return m.os.fds.alloc(Fd {
            kind: FdKind::Dev(d),
            cloexec: flags & O_CLOEXEC != 0,
            flags: flags & !O_CLOEXEC,
        }) as i64;
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
        let fd = Fd {
            kind: FdKind::Dir {
                path: host,
                entries,
                pos: 0,
            },
            cloexec: flags & O_CLOEXEC != 0,
            flags,
        };
        return m.os.fds.alloc(fd) as i64;
    }
    if flags & O_DIRECTORY != 0 {
        return -ENOTDIR;
    }

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
        // Windows 没有 Unix 权限位，只能忽略（已知差异）。
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opt.mode(mode & 0o777);
        }
        #[cfg(not(unix))]
        {
            let _ = mode;
        }
    }
    match opt.open(&host) {
        Ok(f) => {
            let fd = Fd {
                kind: FdKind::File(f),
                cloexec: flags & O_CLOEXEC != 0,
                flags,
            };
            m.os.fds.alloc(fd) as i64
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
fn write_stat(m: &mut Machine, out: u64, md: &std::fs::Metadata) -> i64 {
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
    let (dev, ino, nlink, mode, uid, gid, blksize, blocks) = {
        use std::os::unix::fs::MetadataExt;
        (
            md.dev(),
            md.ino(),
            md.nlink(),
            md.mode(),
            md.uid(),
            md.gid(),
            md.blksize() as i64,
            md.blocks() as i64,
        )
    };
    // Windows 宿主：没有 inode，用合成值。guest 若靠 (dev, ino) 判断
    // "是不是同一个文件"会失效，这是已知缺口。
    #[cfg(not(unix))]
    let (dev, ino, nlink, mode, uid, gid, blksize, blocks) = (
        1u64,
        1u64,
        1u64,
        // 从文件类型合成 mode：Windows 没有 Unix 权限位。
        if md.is_dir() {
            0o040755u32
        } else if md.file_type().is_symlink() {
            0o120777
        } else {
            0o100644
        },
        0u32,
        0u32,
        4096i64,
        size.div_ceil(512) as i64,
    );

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

fn sys_fstat(m: &mut Machine, fd: i32, out: u64) -> i64 {
    let md = match m.os.fds.get(fd).map(|f| &f.kind) {
        Some(FdKind::File(f)) => f.metadata(),
        Some(FdKind::Dir { path, .. }) => std::fs::metadata(path),
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
        Ok(md) => write_stat(m, out, &md),
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
        Ok(md) => write_stat(m, out, &md),
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

fn sys_access(m: &mut Machine, path_ptr: u64, _mode: i32) -> i64 {
    let path = match guest_path(m, path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if fs::DevKind::from_guest_path(&path).is_some() {
        return 0;
    }
    let host = match m.os.vfs.host_path_confined(&path) {
        Ok(p) => p,
        Err(e) => return e.errno(),
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

    // guest 请求的 prot 可能不含写；但文件映射要先把内容写进去，
    // 所以先按可写建立，填完再收紧。
    m.mem.map(base, len, PROT_READ | PROT_WRITE);
    m.mem.zero(base, len);

    if flags & MAP_ANONYMOUS == 0 {
        // 文件映射：把内容读进来。这是"快照式"映射——不是真共享，
        // 对 MAP_SHARED 的写不会回写到文件。已知缺口，见 crate 文档。
        let mut buf = vec![0u8; len as usize];
        let got = match m.os.fds.get_mut(fd).map(|f| &mut f.kind) {
            Some(FdKind::File(f)) => {
                let cur = f.stream_position().ok();
                let r = f
                    .seek(SeekFrom::Start(off as u64))
                    .and_then(|_| read_up_to(f, &mut buf));
                if let Some(c) = cur {
                    let _ = f.seek(SeekFrom::Start(c));
                }
                r
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

fn sys_munmap(m: &mut Machine, addr: u64, len: u64) -> i64 {
    if addr & PAGE_MASK != 0 {
        return -EINVAL;
    }
    m.mem.unmap(addr, (len + PAGE_MASK) & !PAGE_MASK);
    0
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
    // 缩小：原地截掉尾部
    if new_len <= old_len {
        m.mem.unmap(old + new_len, old_len - new_len);
        return old as i64;
    }
    // 扩大：先试原地
    let grow = new_len - old_len;
    if flags & MREMAP_FIXED == 0 && !m.mem.is_mapped(old + old_len, grow) {
        m.mem.map(old + old_len, grow, PROT_READ | PROT_WRITE);
        m.mem.zero(old + old_len, grow);
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
    m.mem.map(dst, new_len, PROT_READ | PROT_WRITE);
    m.mem.zero(dst, new_len);
    m.mem.write_raw(dst, &buf);
    m.mem.unmap(old, old_len);
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

/// `getrlimit`/`prlimit64`。报的值要和 `stack.rs` 里真实建的栈大小一致。
fn sys_getrlimit(m: &mut Machine, nr: u64, a: &[u64; 6]) -> i64 {
    const RLIMIT_STACK: u64 = 3;
    const RLIMIT_NOFILE: u64 = 7;
    const RLIM_INFINITY: u64 = u64::MAX;
    // getrlimit(res, out) / prlimit64(pid, res, new, out)
    let (res, out) = if nr == 97 { (a[0], a[1]) } else { (a[1], a[3]) };
    if out == 0 {
        return 0; // prlimit64 只设不取
    }
    let (cur, max) = match res {
        RLIMIT_STACK => (crate::stack::STACK_SIZE, crate::stack::STACK_SIZE),
        RLIMIT_NOFILE => (1024, 4096),
        _ => (RLIM_INFINITY, RLIM_INFINITY),
    };
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

fn sys_link(m: &mut Machine, old_ptr: u64, new_ptr: u64) -> i64 {
    let (old, new) = match (guest_path(m, old_ptr), guest_path(m, new_ptr)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    // link(2) 不跟随末段：老路径是链接就硬链到链接本身，新路径更不能跟随
    // （跟随会跑去目标位置建链接）。
    let (oh, nh) = (
        m.os.vfs.host_path_nofollow(&old),
        m.os.vfs.host_path_nofollow(&new),
    );
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

fn sys_truncate(m: &mut Machine, path_ptr: u64, len: i64) -> i64 {
    let path = match guest_path(m, path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
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
        // 标准流与管道没有"落盘"这回事，报成功
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
    let rd = m.os.fds.alloc(Fd {
        kind: rk,
        cloexec,
        flags: flags & !O_CLOEXEC,
    });
    let wr = m.os.fds.alloc(Fd {
        kind: wk,
        cloexec,
        flags: flags & !O_CLOEXEC,
    });
    if m.mem.write_u32(fds_ptr, rd as u32).is_err()
        || m.mem.write_u32(fds_ptr + 4, wr as u32).is_err()
    {
        return -EFAULT;
    }
    0
}

/// `poll`。`struct pollfd { int fd; short events; short revents; }`（8 字节）。
///
/// 单线程模拟器里没有"等待"可言：普通文件与标准流永远就绪，管道按缓冲区
/// 是否有数据判断。**不实现阻塞**——真去阻塞在单线程里必然死锁。
fn sys_poll(m: &mut Machine, fds_ptr: u64, nfds: u64, _timeout: i32) -> i64 {
    const POLLIN: u16 = 0x001;
    const POLLOUT: u16 = 0x004;
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
        let revents = match m.os.fds.get(fd).map(|f| &f.kind) {
            None => POLLNVAL,
            Some(FdKind::Dev(_)) => (POLLIN | POLLOUT) & events,
            Some(FdKind::PipeWrite(_)) => POLLOUT & events,
            // 读端：有数据就绪，写端全关也算"就绪"（读到 EOF 而不是挂住），
            // 这正是 POSIX 对已关闭管道的 POLLIN 语义。
            Some(FdKind::PipeRead(inner)) => {
                if !inner.data.borrow().is_empty() || inner.writers_closed() {
                    POLLIN & events
                } else {
                    0
                }
            }
            Some(_) => (POLLIN | POLLOUT) & events,
        };
        if m.mem.write_u16(base + 6, revents).is_err() {
            return -EFAULT;
        }
        if revents != 0 {
            ready += 1;
        }
    }
    ready
}

/// `statx`。只填 guest 常用的字段；`stx_mask` 如实回报我们填了哪些。
/// 布局见 `struct statx`（256 字节）。
fn sys_statx(m: &mut Machine, dirfd: i32, path_ptr: u64, flags: i32, out: u64) -> i64 {
    const STATX_BASIC_STATS: u32 = 0x07ff;
    // 先借 stat 拿到宿主元数据（复用同一套 dirfd/AT_EMPTY_PATH 语义）
    let mut st = [0u8; 144];
    let scratch = 0u64; // 不写 guest 内存，直接在宿主侧取
    let _ = scratch;
    let md = {
        let path = match guest_path(m, path_ptr) {
            Ok(p) => p,
            Err(e) => return e,
        };
        if flags & AT_EMPTY_PATH != 0 && path.is_empty() {
            match m.os.fds.get(dirfd).map(|f| &f.kind) {
                Some(FdKind::File(f)) => f.metadata(),
                Some(FdKind::Dir { path, .. }) => std::fs::metadata(path),
                Some(_) => return -EBADF,
                None => return -EBADF,
            }
        } else {
            // statx 由 AT_SYMLINK_NOFOLLOW 决定跟不跟随。
            let host = match resolve_at(m, dirfd, &path, flags & AT_SYMLINK_NOFOLLOW == 0) {
                Ok(p) => p,
                Err(e) => return e,
            };
            if flags & AT_SYMLINK_NOFOLLOW != 0 {
                std::fs::symlink_metadata(&host)
            } else {
                std::fs::metadata(&host)
            }
        }
    };
    let md = match md {
        Ok(v) => v,
        Err(e) => return host_err(&e),
    };
    let _ = &mut st;

    let size = md.len();
    let mode: u16 = if md.is_dir() {
        0o040755
    } else if md.file_type().is_symlink() {
        0o120777
    } else {
        0o100644
    };
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
            (1u32, 1u64, 4096u32, size.div_ceil(512), 0u32, 0u32)
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
    if !m.mem.is_mapped(addr, (len + PAGE_MASK) & !PAGE_MASK) {
        return -ENOMEM;
    }
    0
}
