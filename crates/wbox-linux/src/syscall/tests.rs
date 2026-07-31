//! syscall 层单测。
//!
//! 这里只放**不需要真 guest 程序**就能验证的语义（内存管理、结构体布局、
//! 错误映射）。端到端的 syscall 行为由 `tests/guest_elf.rs` 用真 ELF 覆盖。

use super::*;
use crate::machine::Machine;
use crate::mem::{PAGE_SIZE, PROT_READ, PROT_WRITE};

fn mach() -> Machine {
    Machine::new(Os::new())
}

/// 建一页可读写的暂存区，返回其地址。
fn scratch(m: &mut Machine) -> u64 {
    let at = 0x5_0000;
    m.mem.map(at, PAGE_SIZE, PROT_READ | PROT_WRITE);
    at
}

#[test]
fn sig_ign_prevents_termination_and_discards_pending_signal() {
    const SIGUSR1: i32 = 10;
    const SIG_IGN: u64 = 1;

    let mut m = mach();
    let at = scratch(&mut m);
    m.os.sig_blocked = sigset_bit(SIGUSR1);
    let pid = m.os.pid;
    assert_eq!(process::sys_kill(&mut m, pid, SIGUSR1).unwrap(), 0);
    assert!(m.os.sig_pending.iter().any(|(s, _, _)| *s == SIGUSR1));

    m.mem.write_u64(at, SIG_IGN).unwrap();
    assert_eq!(sys_rt_sigaction(&mut m, SIGUSR1, at, 0), 0);
    assert!(!m.os.sig_pending.iter().any(|(s, _, _)| *s == SIGUSR1));

    assert_eq!(process::sys_kill(&mut m, pid, SIGUSR1).unwrap(), 0);
    assert!(
        !m.os.sig_pending.iter().any(|(s, _, _)| *s == SIGUSR1),
        "SIG_IGN must discard the signal even while it is blocked"
    );
}

#[test]
fn exec_reset_preserves_ignored_dispositions_only() {
    const SIGUSR1: usize = 10;
    const SIGUSR2: usize = 12;

    let mut m = mach();
    m.os.sig_handlers[SIGUSR1] = 0x1234;
    m.os.sig_handlers[SIGUSR2] = 1;
    m.os.sig_blocked = sigset_bit(SIGUSR1 as i32);
    m.os.raise_signal(SIGUSR1 as i32, 0, m.os.pid);
    let blocked = m.os.sig_blocked;
    let pending = m.os.sig_pending.clone();

    m.os.reset_caught_signal_handlers_for_exec();

    assert_eq!(m.os.sig_handlers[SIGUSR1], 0);
    assert_eq!(m.os.sig_handlers[SIGUSR2], 1);
    assert_eq!(m.os.sig_blocked, blocked);
    assert_eq!(m.os.sig_pending, pending);
}

#[test]
fn setitimer_null_disarms_after_reporting_old_timer() {
    let mut m = mach();
    let at = scratch(&mut m);
    m.os.alarm_interval_ns = 2_000_000_000;
    m.os.alarm_deadline_ns = now_ns() + 5_000_000_000;

    assert_eq!(sys_setitimer(&mut m, 0, 0, at), 0);
    assert_eq!(m.mem.read_u64(at).unwrap(), 2);
    assert!(m.mem.read_u64(at + 16).unwrap() <= 5);
    assert!(m.mem.read_u64(at + 16).unwrap() >= 4);
    assert_eq!(m.os.alarm_interval_ns, 0);
    assert_eq!(m.os.alarm_deadline_ns, 0);
}

#[cfg(windows)]
#[test]
fn fstat_distinguishes_different_windows_files() {
    let dir = std::env::temp_dir().join(format!(
        "wbox-linux-fstat-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let first = dir.join("first");
    let second = dir.join("second");
    std::fs::write(&first, b"a").unwrap();
    std::fs::write(&second, b"b").unwrap();

    let mut m = mach();
    let at = scratch(&mut m);
    let first_fd =
        m.os.fds
            .alloc(fs::Fd::new(
                FdKind::File(std::fs::File::open(&first).unwrap()),
                false,
                0,
            ))
            .unwrap();
    let second_fd =
        m.os.fds
            .alloc(fs::Fd::new(
                FdKind::File(std::fs::File::open(&second).unwrap()),
                false,
                0,
            ))
            .unwrap();

    assert_eq!(sys_fstat(&mut m, first_fd, at), 0);
    assert_eq!(sys_fstat(&mut m, second_fd, at + 144), 0);
    let first_identity = (m.mem.read_u64(at).unwrap(), m.mem.read_u64(at + 8).unwrap());
    let second_identity = (
        m.mem.read_u64(at + 144).unwrap(),
        m.mem.read_u64(at + 152).unwrap(),
    );
    assert_ne!(
        first_identity, second_identity,
        "glibc uses (st_dev, st_ino) to deduplicate loaded shared objects"
    );

    drop(m);
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(windows)]
#[test]
fn windows_ocreat_mode_is_guest_owned_across_metadata_views() {
    let dir = std::env::temp_dir().join(format!(
        "wbox-linux-mode-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();

    let mut m = mach();
    m.os.vfs = fs::Vfs {
        prefix: Some(dir.clone()),
        cwd: std::path::PathBuf::from("/"),
        mounts: Vec::new(),
    };
    m.os.umask = 0o027;
    let at = scratch(&mut m);
    m.mem.write_raw(at, b"/created\0");
    let fd = sys_openat(&mut m, AT_FDCWD, at, O_RDWR | O_CREAT | O_EXCL, 0o666);
    assert!(fd >= 0, "O_CREAT failed: {fd}");
    let fd = fd as i32;
    let expected = 0o640;

    assert_eq!(sys_fstat(&mut m, fd, at + 256), 0);
    assert_eq!(
        m.mem.read_u32(at + 256 + 24).unwrap() & 0o777,
        expected,
        "fstat must report mode & !guest_umask"
    );
    assert_eq!(sys_stat_path(&mut m, AT_FDCWD, at, at + 512, true), 0);
    assert_eq!(
        m.mem.read_u32(at + 512 + 24).unwrap() & 0o777,
        expected,
        "path stat must agree with fstat"
    );
    assert_eq!(sys_statx(&mut m, AT_FDCWD, at, 0, at + 768), 0);
    assert_eq!(
        m.mem.read_u16(at + 768 + 28).unwrap() as u32 & 0o777,
        expected,
        "statx must agree with fstat"
    );
    let reopened = sys_openat(&mut m, AT_FDCWD, at, O_RDWR | O_CREAT, 0o777);
    assert!(
        reopened >= 0,
        "reopening an existing file failed: {reopened}"
    );
    assert_eq!(sys_fstat(&mut m, reopened as i32, at + 896), 0);
    assert_eq!(
        m.mem.read_u32(at + 896 + 24).unwrap() & 0o777,
        expected,
        "O_CREAT on an existing inode must not replace its mode"
    );
    assert_eq!(sys_close(&mut m, reopened as i32), 0);

    let renamed = dir.join("renamed");
    std::fs::rename(dir.join("created"), &renamed).unwrap();
    std::fs::hard_link(&renamed, dir.join("linked")).unwrap();
    for (name, out) in [
        (b"/renamed\0".as_slice(), at + 1024),
        (b"/linked\0", at + 1280),
    ] {
        m.mem.write_raw(at, name);
        assert_eq!(sys_stat_path(&mut m, AT_FDCWD, at, out, true), 0);
        assert_eq!(
            m.mem.read_u32(out + 24).unwrap() & 0o777,
            expected,
            "mode must follow the Windows file identity across rename/hardlink"
        );
    }

    drop(m);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn brk_grows_and_maps_new_pages() {
    let mut m = mach();
    m.mem.brk = 0x10_0000;
    m.mem.brk_start = 0x10_0000;

    // 查询：传 0 返回当前 brk，不改变任何东西
    assert_eq!(sys_brk(&mut m, 0), 0x10_0000);
    assert!(!m.mem.is_mapped(0x10_0000, PAGE_SIZE));

    // 扩展：新区间必须可读写且已清零
    let want = 0x10_0000 + PAGE_SIZE * 2;
    assert_eq!(sys_brk(&mut m, want), want as i64);
    assert!(m.mem.is_mapped(0x10_0000, PAGE_SIZE * 2));
    assert_eq!(m.mem.read_u64(0x10_0000).unwrap(), 0);
    assert!(m.mem.write_u64(0x10_0000, 1).is_ok());
}

#[test]
fn brk_shrink_unmaps_and_below_start_is_ignored() {
    let mut m = mach();
    m.mem.brk = 0x10_0000;
    m.mem.brk_start = 0x10_0000;
    let want = 0x10_0000 + PAGE_SIZE * 4;
    sys_brk(&mut m, want);
    assert!(m.mem.is_mapped(want - PAGE_SIZE, PAGE_SIZE));

    // 收缩：高处的页要被撤销
    sys_brk(&mut m, 0x10_0000 + PAGE_SIZE);
    assert!(!m.mem.is_mapped(want - PAGE_SIZE, PAGE_SIZE));

    // 低于 brk_start 的请求按查询处理，不得把起始页也撤掉
    let cur = m.mem.brk;
    assert_eq!(sys_brk(&mut m, 0x1000), cur as i64);
}

#[test]
fn uname_fills_six_65_byte_fields() {
    let mut m = mach();
    let at = scratch(&mut m);
    assert_eq!(sys_uname(&mut m, at), 0);
    // guest 读到的是 6 个定长 65 字节字段，逐个按 NUL 结尾解析
    let read = |off: u64| -> String {
        String::from_utf8_lossy(&m.mem.read_cstr(at + off, 65).unwrap()).into_owned()
    };
    assert_eq!(read(0), "Linux", "sysname 必须是 Linux");
    assert_eq!(read(65 * 4), "x86_64", "machine 必须是 x86_64");
    // release 要足够新：glibc 有内核版本下限检查，报太老会直接 "kernel too old"
    let release = read(65 * 2);
    let major: u32 = release.split('.').next().unwrap().parse().unwrap();
    assert!(major >= 3, "release={release} 对 glibc 来说太老");
}

#[test]
fn uname_rejects_unmapped_buffer() {
    let mut m = mach();
    assert_eq!(sys_uname(&mut m, 0xdead_0000), -EFAULT);
}

#[test]
fn mmap_anonymous_honours_requested_prot() {
    let mut m = mach();
    // PROT_READ only（1），匿名（MAP_ANONYMOUS=0x20 | MAP_PRIVATE=0x2）
    let a = sys_mmap(&mut m, 0, PAGE_SIZE, 1, 0x22, -1, 0);
    assert!(a > 0, "mmap 应返回地址，实际 {a}");
    let a = a as u64;
    assert!(m.mem.read_u8(a).is_ok());
    assert!(
        m.mem.write_u8(a, 1).is_err(),
        "只请求了 PROT_READ，不该可写"
    );
    // 内容必须清零——匿名映射的 Linux 语义
    assert_eq!(m.mem.read_u8(a).unwrap(), 0);
}

#[test]
fn mmap_rejects_zero_length_and_unaligned_fixed() {
    let mut m = mach();
    assert_eq!(sys_mmap(&mut m, 0, 0, 3, 0x22, -1, 0), -EINVAL);
    // MAP_FIXED(0x10) 且地址未页对齐
    assert_eq!(sys_mmap(&mut m, 0x1001, PAGE_SIZE, 3, 0x32, -1, 0), -EINVAL);
}

#[test]
fn mprotect_requires_mapped_range_and_changes_access() {
    let mut m = mach();
    let a = sys_mmap(&mut m, 0, PAGE_SIZE, 3, 0x22, -1, 0) as u64;
    assert!(m.mem.write_u8(a, 0x5a).is_ok());
    // 收紧成只读
    assert_eq!(sys_mprotect(&mut m, a, PAGE_SIZE, 1), 0);
    assert!(m.mem.write_u8(a, 0).is_err());
    assert_eq!(m.mem.read_u8(a).unwrap(), 0x5a, "改权限不该丢内容");
    // 未映射区间要报 ENOMEM，而不是假装成功
    assert_eq!(sys_mprotect(&mut m, 0xdead_0000, PAGE_SIZE, 3), -ENOMEM);
}

#[test]
fn munmap_makes_range_inaccessible() {
    let mut m = mach();
    let a = sys_mmap(&mut m, 0, PAGE_SIZE, 3, 0x22, -1, 0) as u64;
    assert_eq!(sys_munmap(&mut m, a, PAGE_SIZE), 0);
    assert!(m.mem.read_u8(a).is_err());
}

#[test]
fn mremap_grows_in_place_and_preserves_contents() {
    let mut m = mach();
    let a = sys_mmap(&mut m, 0, PAGE_SIZE, 3, 0x22, -1, 0) as u64;
    m.mem.write_u32(a, 0xabcd_1234).unwrap();
    // MREMAP_MAYMOVE=1
    let r = sys_mremap(&mut m, a, PAGE_SIZE, PAGE_SIZE * 2, 1, 0);
    assert!(r > 0, "mremap 失败：{r}");
    let r = r as u64;
    assert_eq!(m.mem.read_u32(r).unwrap(), 0xabcd_1234, "搬移后内容要保留");
    assert!(m.mem.is_mapped(r, PAGE_SIZE * 2));
}

#[test]
fn arch_prctl_sets_and_gets_fs_base() {
    let mut m = mach();
    let at = scratch(&mut m);
    // ARCH_SET_FS = 0x1002
    assert_eq!(sys_arch_prctl(&mut m, 0x1002, 0xdead_beef), 0);
    assert_eq!(m.cpu.fs_base, 0xdead_beef);
    // ARCH_GET_FS = 0x1003
    assert_eq!(sys_arch_prctl(&mut m, 0x1003, at), 0);
    assert_eq!(m.mem.read_u64(at).unwrap(), 0xdead_beef);
    // 未知 code 必须报错
    assert_eq!(sys_arch_prctl(&mut m, 0x9999, 0), -EINVAL);
}

#[test]
fn getrlimit_stack_matches_the_stack_we_actually_build() {
    let mut m = mach();
    let at = scratch(&mut m);
    // getrlimit(RLIMIT_STACK=3, out)
    let a = [3u64, at, 0, 0, 0, 0];
    assert_eq!(sys_rlimit(&mut m, 97, &a), 0);
    // 报的值必须和 stack::setup 真实建的栈一致，否则 guest 会算错栈边界
    assert_eq!(m.mem.read_u64(at).unwrap(), crate::stack::STACK_SIZE);
}

#[test]
fn write_to_bad_fd_is_ebadf() {
    let mut m = mach();
    let at = scratch(&mut m);
    m.mem.write(at, b"hi").unwrap();
    assert_eq!(sys_write(&mut m, 99, at, 2), -EBADF);
}

#[test]
fn write_rejects_unmapped_guest_buffer() {
    let mut m = mach();
    // 源缓冲区不可读 -> EFAULT，而不是把宿主内存里的东西写出去
    assert_eq!(sys_write(&mut m, 1, 0xdead_0000, 16), -EFAULT);
}

#[test]
fn read_at_eof_returns_zero_without_touching_the_buffer() {
    let mut m = mach();
    // 注意这里**不是** EFAULT：stdin 在测试环境下立刻 EOF，读到 0 字节就
    // 根本不会去拷贝，Linux 同样返回 0 而不报错。只有真的要写 guest 缓冲区
    // 时才检查可写性（见 write_rejects_unmapped_guest_buffer）。
    assert_eq!(sys_read(&mut m, 0, 0xdead_0000, 16), 0);
}

#[test]
fn getrandom_returns_requested_bytes() {
    let mut m = mach();
    let at = scratch(&mut m);
    let n = sys_getrandom(&mut m, at, 32, 0);
    // 平台没接 CSPRNG 时允许 ENOSYS（宁可报错也不给假随机），
    // 但接上了就必须真填满请求长度。
    if n == -ENOSYS {
        return;
    }
    assert_eq!(n, 32, "getrandom 应返回写入字节数");
    let mut buf = [0u8; 32];
    m.mem.read(at, &mut buf).unwrap();
    assert!(
        buf.iter().any(|&b| b != 0),
        "32 字节全 0 几乎不可能，疑似没真填"
    );
}

#[test]
fn unknown_syscall_number_is_enosys() {
    // 直接走 dispatch：rax = 一个我们没实现的号
    let mut m = mach();
    m.cpu.regs[RAX] = 9999;
    dispatch(&mut m, 0x1000).unwrap();
    assert_eq!(m.cpu.regs[RAX] as i64, -ENOSYS);
}

#[test]
fn host_err_maps_not_found_to_enoent() {
    let e = std::io::Error::new(std::io::ErrorKind::NotFound, "x");
    assert_eq!(host_err(&e), -ENOENT);
    let e = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "x");
    assert_eq!(host_err(&e), -EACCES);
}

// ---------------------------------------------------------------- 管道与 EOF

/// 建一对管道，返回 (读 fd, 写 fd)。
fn pipe_pair(m: &mut Machine) -> (i32, i32) {
    let at = scratch(m);
    assert_eq!(sys_pipe(m, at, 0), 0);
    (
        m.mem.read_u32(at).unwrap() as i32,
        m.mem.read_u32(at + 4).unwrap() as i32,
    )
}

#[test]
fn pipe_read_returns_eagain_while_a_writer_is_open_and_eof_after() {
    let mut m = mach();
    let (rd, wr) = pipe_pair(&mut m);
    let at = scratch(&mut m) + 64;

    // 空管道 + 写端还开着 = EAGAIN（单线程下阻塞必然死锁，见 sys_read）
    assert_eq!(sys_read(&mut m, rd, at, 4), -EAGAIN);

    m.mem.write_raw(at, b"hey");
    assert_eq!(sys_write(&mut m, wr, at, 3), 3);
    assert_eq!(sys_read(&mut m, rd, at + 16, 3), 3);
    let mut got = [0u8; 3];
    m.mem.read(at + 16, &mut got).unwrap();
    assert_eq!(&got, b"hey");

    // 写端关掉之后，空管道 = EOF（返回 0）。这一条是 `$(cmd)` 能收敛的前提：
    // 若继续报 EAGAIN，shell 会在读端上无限自旋。
    assert_eq!(sys_close(&mut m, wr), 0);
    assert_eq!(sys_read(&mut m, rd, at, 4), 0);
}

#[test]
fn pipe_writer_count_survives_dup_and_drops_with_the_last_fd() {
    let mut m = mach();
    let (rd, wr) = pipe_pair(&mut m);
    let at = scratch(&mut m) + 64;

    let alias = sys_dup(&mut m, wr);
    assert!(alias >= 0, "管道必须能 dup（shell 的重定向就靠它）");
    // 关掉原来的写端，别名还在 -> 还不是 EOF
    assert_eq!(sys_close(&mut m, wr), 0);
    assert_eq!(sys_read(&mut m, rd, at, 4), -EAGAIN);
    // 最后一个写端也关掉 -> EOF
    assert_eq!(sys_close(&mut m, alias as i32), 0);
    assert_eq!(sys_read(&mut m, rd, at, 4), 0);
}

#[test]
fn pipes_are_not_seekable_and_not_p_readable() {
    let mut m = mach();
    let (rd, wr) = pipe_pair(&mut m);
    let at = scratch(&mut m) + 64;
    // Linux 对管道给 ESPIPE，不是 EBADF——guest 靠这个区分"不能 seek"
    // 和"fd 是坏的"。
    assert_eq!(sys_lseek(&mut m, rd, 0, 1), -ESPIPE);
    assert_eq!(sys_pread(&mut m, rd, at, 1, 0), -ESPIPE);
    assert_eq!(sys_pwrite(&mut m, wr, at, 1, 0), -ESPIPE);
    // 反向使用两端仍然是 EBADF
    assert_eq!(sys_read(&mut m, wr, at, 1), -EBADF);
    assert_eq!(sys_write(&mut m, rd, at, 1), -EBADF);
}

#[test]
fn bad_fd_is_ebadf_even_when_the_iovec_is_empty() {
    let mut m = mach();
    let at = scratch(&mut m);
    // iov 数量为 0 时循环一次都不跑，必须在进循环前校验 fd。
    assert_eq!(sys_readv(&mut m, 9999, at, 0), -EBADF);
    assert_eq!(sys_writev(&mut m, 9999, at, 0), -EBADF);
    assert_eq!(sys_preadv(&mut m, 9999, at, 0, 0), -EBADF);
    assert_eq!(sys_pwritev(&mut m, 9999, at, 0, 0), -EBADF);
}

// ------------------------------------------------------------- 合成 /dev

/// 打开一个 guest 路径，返回 fd 或 -errno。
fn open_guest(m: &mut Machine, path: &str, flags: i32) -> i64 {
    let at = 0x9_0000;
    m.mem.map(at, PAGE_SIZE, PROT_READ | PROT_WRITE);
    m.mem.write_raw(at, path.as_bytes());
    m.mem.write_raw(at + path.len() as u64, &[0]);
    sys_openat(m, AT_FDCWD, at, flags, 0)
}

#[test]
fn dev_null_zero_and_full_have_the_documented_semantics() {
    let mut m = mach();
    let buf = scratch(&mut m) + 128;

    let null = open_guest(&mut m, "/dev/null", O_RDWR);
    assert!(null >= 0, "/dev/null 必须能打开（rootfs 里没有 /dev）");
    let null = null as i32;
    assert_eq!(sys_read(&mut m, null, buf, 16), 0, "/dev/null 读出来是 EOF");
    assert_eq!(sys_write(&mut m, null, buf, 16), 16, "写入被丢弃但报成功");

    let zero = open_guest(&mut m, "/dev/zero", O_RDONLY) as i32;
    m.mem.write_raw(buf, &[0xff; 8]);
    assert_eq!(sys_read(&mut m, zero, buf, 8), 8);
    let mut got = [0u8; 8];
    m.mem.read(buf, &mut got).unwrap();
    assert_eq!(got, [0u8; 8], "/dev/zero 必须真的填 0");

    // /dev/full 的全部存在意义就是写入失败
    let full = open_guest(&mut m, "/dev/full", O_RDWR) as i32;
    assert_eq!(sys_read(&mut m, full, buf, 4), 4);
    assert_eq!(sys_write(&mut m, full, buf, 4), -ENOSPC);
}

#[test]
fn dev_nodes_stat_as_character_devices() {
    let mut m = mach();
    let at = 0x9_0000;
    m.mem.map(at, PAGE_SIZE, PROT_READ | PROT_WRITE);
    m.mem.write_raw(at, b"/dev/null\0");
    let out = at + 256;
    assert_eq!(sys_stat_path(&mut m, AT_FDCWD, at, out, true), 0);
    let mode = m.mem.read_u32(out + 24).unwrap();
    const S_IFMT: u32 = 0o170000;
    const S_IFCHR: u32 = 0o020000;
    assert_eq!(mode & S_IFMT, S_IFCHR, "test -c /dev/null 靠这一位");
    // access 也必须说"存在"
    assert_eq!(sys_faccessat(&mut m, AT_FDCWD, at, 0), 0);
}

#[test]
fn empty_path_is_enoent_not_the_current_directory() {
    let mut m = mach();
    // 不显式判空，`open("")` 会被当成 cwd 并成功返回一个目录 fd。
    assert_eq!(open_guest(&mut m, "", O_RDONLY), -ENOENT);
}

// ------------------------------------------------------------ 进程族记账

#[test]
fn wait4_reports_recorded_children_and_then_echild() {
    let mut m = mach();
    let at = scratch(&mut m);
    // 没有子进程：ECHILD（而不是挂住）
    assert_eq!(process::sys_wait4(&mut m, -1, at, 0), -ECHILD);

    // 快照式 fork 下子进程在 fork 返回前就跑完了，僵尸表里直接记账。
    m.os.zombies.push(process::ZombieChild {
        pid: 7,
        status: 42 << 8,
    });
    m.os.zombies
        .push(process::ZombieChild { pid: 8, status: 9 });

    // 指定 pid 收指定的那个
    assert_eq!(process::sys_wait4(&mut m, 8, at, 0), 8);
    assert_eq!(m.mem.read_u32(at).unwrap(), 9, "被信号杀：低 7 位是信号号");

    // -1 收任意一个
    assert_eq!(process::sys_wait4(&mut m, -1, at, 0), 7);
    assert_eq!(
        m.mem.read_u32(at).unwrap(),
        42 << 8,
        "正常退出：码在 8..16 位"
    );

    // 收干净之后再 wait 是 ECHILD
    assert_eq!(process::sys_wait4(&mut m, -1, at, 0), -ECHILD);
    // 不是自己孩子的 pid 也是 ECHILD
    m.os.zombies
        .push(process::ZombieChild { pid: 7, status: 0 });
    assert_eq!(process::sys_wait4(&mut m, 999, at, 0), -ECHILD);
}

#[test]
fn wait4_rejects_unknown_options_and_bad_status_pointers() {
    let mut m = mach();
    m.os.zombies
        .push(process::ZombieChild { pid: 3, status: 0 });
    assert_eq!(process::sys_wait4(&mut m, -1, 0, 0x4000), -EINVAL);
    // 坏指针：账不能被吞掉，否则这个子进程永远收不回来
    assert_eq!(process::sys_wait4(&mut m, -1, 0xdead_0000, 0), -EFAULT);
    assert_eq!(m.os.zombies.len(), 1, "写状态失败时僵尸必须留在表里");
}

#[test]
fn clone_with_thread_flags_is_enosys_not_a_silent_fork() {
    let mut m = mach();
    // CLONE_VM|CLONE_THREAD|CLONE_SIGHAND = 线程创建。假装成功会让 guest
    // 以为多了一个执行流，之后的行为完全不可预测。
    assert_eq!(
        process::sys_clone(&mut m, 0x1_0000 | 0x100 | 0x800, 0, 0),
        -ENOSYS
    );
}

#[test]
fn pids_are_unique_across_the_whole_process_tree() {
    let m = mach();
    let a = m.os.alloc_pid();
    let b = m.os.alloc_pid();
    assert_ne!(a, b);
    // 子进程共享同一个计数器：不共享的话父子会各自发出重复的 pid。
    let child = m.os.clone_for_fork(m.os.fds.try_clone().unwrap(), a);
    assert_ne!(child.alloc_pid(), b);
    assert!(child.alloc_pid() > b);
    assert_eq!(child.ppid, m.os.pid);
    assert_eq!(child.fork_depth, m.os.fork_depth + 1);
}
