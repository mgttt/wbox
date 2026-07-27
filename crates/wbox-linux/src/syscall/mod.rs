//! Linux x86-64 syscall 模拟。
//!
//! 约定：每个 `sys_*` 返回 `i64`，负值是 `-errno`（Linux 内核的 ABI）。
//! guest 侧的 libc 会自己把负值翻成 `errno`。
//!
//! **没实现的 syscall 一律返回 `-ENOSYS` 并在 `WBOX_STRACE=1` 下打印**，
//! 绝不假装成功——假装成功会让 guest 在离故障点很远的地方以莫名其妙的方式坏掉。

pub mod fs;

use crate::cpu::{RAX, RCX, RDX, RSI, RDI, R8, R9, R10, R11};
use crate::machine::{ExecResult, Exception, Machine};
use crate::mem::{PAGE_MASK, PROT_EXEC, PROT_READ, PROT_WRITE};
use fs::{Fd, FdKind, FdTable, Vfs};
use std::io::{Read, Seek, SeekFrom, Write};

// ---------------------------------------------------------------- errno
pub const EPERM: i64 = 1;
pub const EIO: i64 = 5;
pub const ENOENT: i64 = 2;
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

const AT_FDCWD: i32 = -100;
/// `newfstatat` 的 `AT_EMPTY_PATH`：路径为空时对 fd 本身取状态。
const AT_EMPTY_PATH: i32 = 0x1000;
const AT_SYMLINK_NOFOLLOW: i32 = 0x100;

const PATH_MAX: usize = 4096;

/// guest 侧可见的 OS 状态。
pub struct Os {
    pub fds: FdTable,
    pub vfs: Vfs,
    /// guest 进程号。模拟器是单进程，报一个固定值即可。
    pub pid: i32,
    /// `set_tid_address` 记下的地址，线程退出时要清零（当前单线程，仅存不用）。
    pub tid_address: u64,
    /// `WBOX_STRACE=1`：把每次 syscall 打到 stderr。
    pub strace: bool,
    /// `uname` 报的内核版本。
    pub release: String,
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
            tid_address: 0,
            strace: std::env::var_os("WBOX_STRACE").is_some_and(|v| v != "0"),
            // 报一个足够新的版本：glibc 会检查内核版本下限，报太老会直接
            // "FATAL: kernel too old" 退出。
            release: "6.1.0-wbox".to_string(),
        }
    }
}

/// 把宿主 `io::Error` 翻成 `-errno`。
fn host_err(e: &std::io::Error) -> i64 {
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
        eprintln!(
            "wbox-linux: syscall {} ({:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x})",
            nr, a[0], a[1], a[2], a[3], a[4], a[5]
        );
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
        9 => sys_mmap(m, a[0], a[1], a[2] as i32, a[3] as i32, a[4] as i32, a[5] as i64),
        10 => sys_mprotect(m, a[0], a[1], a[2] as i32),
        11 => sys_munmap(m, a[0], a[1]),
        12 => sys_brk(m, a[0]),
        // 信号：模拟器目前不投递信号，登记动作直接成功（guest 装了处理器
        // 但永远收不到，行为等价于"没有信号发生"）。
        13 | 14 | 131 => 0,
        16 => sys_ioctl(m, a[0] as i32, a[1], a[2]),
        17 => sys_pread(m, a[0] as i32, a[1], a[2], a[3] as i64),
        19 => sys_readv(m, a[0] as i32, a[1], a[2]),
        20 => sys_writev(m, a[0] as i32, a[1], a[2]),
        21 => sys_access(m, a[0], a[1] as i32),
        24 => 0, // sched_yield：单线程下无事可做
        25 => sys_mremap(m, a[0], a[1], a[2], a[3] as i32, a[4]),
        28 => 0,  // madvise：建议性，忽略即为正确实现
        32 => sys_dup(m, a[0] as i32),
        33 => sys_dup2(m, a[0] as i32, a[1] as i32),
        39 | 102 | 104 | 107 | 108 | 110 | 111 => match nr {
            39 => m.os.pid as i64,  // getpid
            110 => 0,               // getppid
            111 => m.os.pid as i64, // getpgrp
            _ => 0,                 // getuid/getgid/geteuid/getegid：容器内是 root
        },
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
        _ => {
            if m.os.strace {
                eprintln!("wbox-linux: syscall {nr} 未实现 -> ENOSYS");
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
fn resolve_at(m: &Machine, dirfd: i32, path: &str) -> Result<std::path::PathBuf, i64> {
    if path.starts_with('/') || dirfd == AT_FDCWD {
        return Ok(m.os.vfs.host_path(path));
    }
    match m.os.fds.get(dirfd).map(|f| &f.kind) {
        Some(FdKind::Dir { path: dir, .. }) => Ok(dir.join(path)),
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
            o.write_all(data).and_then(|_| o.flush()).map(|_| data.len())
        }
        Some(FdKind::Stderr) => {
            let mut o = std::io::stderr();
            o.write_all(data).and_then(|_| o.flush()).map(|_| data.len())
        }
        Some(FdKind::File(f)) => f.write(data),
        Some(FdKind::Dir { .. }) => return -EBADF,
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
        // 标准流可能是管道；管道不可 seek
        Some(FdKind::Stdin) | Some(FdKind::Stdout) | Some(FdKind::Stderr) => -ESPIPE,
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
        _ => return -EBADF,
    };
    let flags = m.os.fds.get(fd).map(|f| f.flags).unwrap_or(0);
    // dup/dup2 产生的新 fd **不继承** O_CLOEXEC，这是 POSIX 明确规定的。
    let nf = Fd { kind, cloexec: false, flags };
    match at {
        None => m.os.fds.alloc(nf) as i64,
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
    const F_DUPFD_CLOEXEC: i32 = 1030;
    if !m.os.fds.contains(fd) {
        return -EBADF;
    }
    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            let r = dup_impl(m, fd, None);
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
        _ => -EINVAL,
    }
}

/// `ioctl`。只认 guest 用来判断"是不是 tty"的那几个请求。
fn sys_ioctl(m: &mut Machine, fd: i32, req: u64, _arg: u64) -> i64 {
    const TCGETS: u64 = 0x5401;
    const TIOCGWINSZ: u64 = 0x5413;
    if !m.os.fds.contains(fd) {
        return -EBADF;
    }
    match req {
        // 一律报"不是终端"。这会让 guest 侧的 libc 选行缓冲/全缓冲里的
        // 全缓冲、让 ls 输出单列——语义正确且可预期。真正的 pty 支持
        // 需要宿主侧的伪终端，属于后续里程碑。
        TCGETS | TIOCGWINSZ => -ENOTTY,
        _ => -ENOTTY,
    }
}

// ------------------------------------------------------------- open/stat

fn sys_openat(m: &mut Machine, dirfd: i32, path_ptr: u64, flags: i32, _mode: u32) -> i64 {
    let path = match guest_path(m, path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let host = match resolve_at(m, dirfd, &path) {
        Ok(p) => p,
        Err(e) => return e,
    };

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
            kind: FdKind::Dir { path: host, entries, pos: 0 },
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
        Some(FdKind::Stdin) | Some(FdKind::Stdout) | Some(FdKind::Stderr) => {
            let mut b = [0u8; 144];
            b[24..28].copy_from_slice(&0o020620u32.to_le_bytes()); // S_IFCHR
            b[16..24].copy_from_slice(&1u64.to_le_bytes());
            b[56..64].copy_from_slice(&1024i64.to_le_bytes());
            return if m.mem.write(out, &b).is_err() { -EFAULT } else { 0 };
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
    let host = match resolve_at(m, dirfd, &path) {
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
    let host = m.os.vfs.host_path(&path);
    // 只做存在性判断。真实的 X_OK/W_OK 要看宿主权限位，Windows 上没有
    // 对应语义；容器内一律 root，存在即可访问是可接受的近似。
    if host.exists() {
        0
    } else {
        -ENOENT
    }
}

fn sys_unlinkat(m: &mut Machine, dirfd: i32, path_ptr: u64, flags: i32) -> i64 {
    const AT_REMOVEDIR: i32 = 0x200;
    let path = match guest_path(m, path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let host = match resolve_at(m, dirfd, &path) {
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
    // /proc/self/exe 是 guest 定位自身的常用手段，单独answer。
    if path == "/proc/self/exe" {
        let s = b"/proc/self/exe";
        let n = (size as usize).min(s.len());
        return if m.mem.write(buf, &s[..n]).is_err() {
            -EFAULT
        } else {
            n as i64
        };
    }
    let host = match resolve_at(m, dirfd, &path) {
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
    let norm = m.os.vfs.normalize(&path);
    let host = m.os.vfs.host_path(&path);
    if !host.is_dir() {
        return if host.exists() { -ENOTDIR } else { -ENOENT };
    }
    m.os.vfs.cwd = norm;
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

fn sys_mmap(
    m: &mut Machine,
    addr: u64,
    len: u64,
    prot: i32,
    flags: i32,
    fd: i32,
    off: i64,
) -> i64 {
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

fn sys_mremap(m: &mut Machine, old: u64, old_len: u64, new_len: u64, flags: i32, new_addr: u64) -> i64 {
    if old & PAGE_MASK != 0 || new_len == 0 {
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
            if m.mem.write_u64(addr, v).is_err() { -EFAULT } else { 0 }
        }
        ARCH_GET_GS => {
            let v = m.cpu.gs_base;
            if m.mem.write_u64(addr, v).is_err() { -EFAULT } else { 0 }
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
