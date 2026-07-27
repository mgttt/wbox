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
    assert!(m.mem.write_u8(a, 1).is_err(), "只请求了 PROT_READ，不该可写");
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
    assert_eq!(sys_getrlimit(&mut m, 97, &a), 0);
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
    assert!(buf.iter().any(|&b| b != 0), "32 字节全 0 几乎不可能，疑似没真填");
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
