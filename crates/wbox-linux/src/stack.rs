//! 初始栈构造：argc/argv/envp/auxv。
//!
//! 布局照抄 Linux x86-64 的 `create_elf_tables()`。这块最容易出错也最难查：
//! auxv 少一项或者 `rsp` 没 16 字节对齐，glibc 的启动代码会在 `_dl_start`
//! 里以看不出原因的方式崩掉。

use crate::elf::Loaded;
use crate::mem::{Mem, PROT_READ, PROT_WRITE};

/// 栈顶（不含）。选在 128TB 用户空间的顶端附近，与 Linux 习惯一致。
pub const STACK_TOP: u64 = 0x7fff_ffff_f000;
/// 初始栈大小，8MB（Linux 的默认 `RLIMIT_STACK`）。
pub const STACK_SIZE: u64 = 8 << 20;

// auxv 类型
pub const AT_NULL: u64 = 0;
pub const AT_IGNORE: u64 = 1;
pub const AT_PHDR: u64 = 3;
pub const AT_PHENT: u64 = 4;
pub const AT_PHNUM: u64 = 5;
pub const AT_PAGESZ: u64 = 6;
pub const AT_BASE: u64 = 7;
pub const AT_FLAGS: u64 = 8;
pub const AT_ENTRY: u64 = 9;
pub const AT_UID: u64 = 11;
pub const AT_EUID: u64 = 12;
pub const AT_GID: u64 = 13;
pub const AT_EGID: u64 = 14;
pub const AT_PLATFORM: u64 = 15;
pub const AT_HWCAP: u64 = 16;
pub const AT_CLKTCK: u64 = 17;
pub const AT_SECURE: u64 = 23;
pub const AT_RANDOM: u64 = 25;
pub const AT_HWCAP2: u64 = 26;
pub const AT_EXECFN: u64 = 31;

/// 我们向 guest 宣告的 CPUID 特性集（`AT_HWCAP` = CPUID.1:EDX）。
/// 必须与 `exec.rs` 真正实现的指令集一致：多报了 guest 会用上没实现的指令，
/// 少报了 libc 会退回慢路径（但仍然正确）。
/// 这里报到 SSE2（bit 26）为止：FPU/TSC/CMOV/MMX/FXSR/SSE/SSE2。
pub const HWCAP: u64 = (1 << 0)   // FPU
    | (1 << 4)   // TSC
    | (1 << 8)   // CX8 (cmpxchg8b)
    | (1 << 11)  // SEP (syscall/sysret)
    | (1 << 15)  // CMOV
    | (1 << 23)  // MMX
    | (1 << 24)  // FXSR
    | (1 << 25)  // SSE
    | (1 << 26); // SSE2

/// 构造初始栈，返回入口处的 `rsp`。
pub fn setup(mem: &mut Mem, loaded: &Loaded, argv: &[Vec<u8>], envp: &[Vec<u8>]) -> u64 {
    let stack_low = STACK_TOP - STACK_SIZE;
    mem.map(stack_low, STACK_SIZE, PROT_READ | PROT_WRITE);
    mem.zero(stack_low, STACK_SIZE);

    // 1) 先把字符串区放在栈顶，自上而下堆。
    let mut p = STACK_TOP;

    let push_bytes = |mem: &mut Mem, p: &mut u64, b: &[u8]| -> u64 {
        *p -= b.len() as u64 + 1; // +1 给 NUL
        let at = *p;
        mem.write_raw(at, b);
        mem.write_raw(at + b.len() as u64, &[0]);
        at
    };

    // AT_EXECFN 指向 argv[0] 的一份独立副本（Linux 就是这么做的）。
    let execfn = push_bytes(mem, &mut p, argv.first().map(|v| v.as_slice()).unwrap_or(b""));

    let env_ptrs: Vec<u64> = envp.iter().map(|e| push_bytes(mem, &mut p, e)).collect();
    let arg_ptrs: Vec<u64> = argv.iter().map(|a| push_bytes(mem, &mut p, a)).collect();
    let platform = push_bytes(mem, &mut p, b"x86_64");

    // 2) AT_RANDOM 的 16 字节。glibc 用它初始化栈保护 canary 和指针混淆。
    p -= 16;
    let random_at = p & !0xf;
    p = random_at;
    mem.write_raw(random_at, &random_bytes());

    // 3) auxv。放在指针数组之后，所以先算好，最后统一写。
    let auxv: Vec<(u64, u64)> = vec![
        (AT_PHDR, loaded.phdr_addr),
        (AT_PHENT, loaded.phentsize),
        (AT_PHNUM, loaded.phnum),
        (AT_PAGESZ, crate::mem::PAGE_SIZE),
        (AT_BASE, loaded.interp_base),
        (AT_FLAGS, 0),
        (AT_ENTRY, loaded.prog_entry),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_SECURE, 0),
        (AT_CLKTCK, 100),
        (AT_HWCAP, HWCAP),
        (AT_HWCAP2, 0),
        (AT_PLATFORM, platform),
        (AT_RANDOM, random_at),
        (AT_EXECFN, execfn),
        (AT_NULL, 0),
    ];

    // 4) 算出指针区总大小，对齐后一次性写。
    //    argc + argv[] + NULL + envp[] + NULL + auxv[]*2
    let words = 1 + arg_ptrs.len() + 1 + env_ptrs.len() + 1 + auxv.len() * 2;
    let bytes = words as u64 * 8;
    // 关键：**入口处 rsp 必须 16 字节对齐**。SysV ABI 要求，SSE 的
    // movaps 存取栈上局部变量时依赖它，对不齐会直接 #GP。
    let mut sp = (p - bytes) & !0xf;
    // 对齐后指针区仍要正好落在 [sp, sp+bytes)，所以从 sp 开始顺序写。
    let rsp = sp;

    let put = |mem: &mut Mem, sp: &mut u64, v: u64| {
        mem.write_raw(*sp, &v.to_le_bytes());
        *sp += 8;
    };

    put(mem, &mut sp, arg_ptrs.len() as u64); // argc
    for a in &arg_ptrs {
        put(mem, &mut sp, *a);
    }
    put(mem, &mut sp, 0); // argv NULL
    for e in &env_ptrs {
        put(mem, &mut sp, *e);
    }
    put(mem, &mut sp, 0); // envp NULL
    for (t, v) in &auxv {
        put(mem, &mut sp, *t);
        put(mem, &mut sp, *v);
    }

    debug_assert_eq!(rsp % 16, 0);
    rsp
}

/// AT_RANDOM 的 16 字节种子。
///
/// 不引第三方 crate：用宿主的单调时钟、进程 id 和一个栈地址做混合。
/// 这不是密码学强度的随机——guest 只用它做 canary 与指针混淆，
/// 目的是每次运行不同，而不是不可预测。真需要随机的 guest 会走
/// `getrandom` 系统调用，那条路由宿主的 CSPRNG 提供。
fn random_bytes() -> [u8; 16] {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    let pid = std::process::id() as u64;
    let stack_marker = &nanos as *const u64 as u64;
    let mut out = [0u8; 16];
    let mut s = nanos ^ (pid << 32) ^ stack_marker;
    for chunk in out.chunks_mut(8) {
        // splitmix64
        s = s.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        chunk.copy_from_slice(&z.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded() -> Loaded {
        Loaded {
            entry: 0x1000,
            prog_entry: 0x1000,
            bias: 0,
            phdr_addr: 0x400040,
            phentsize: 56,
            phnum: 9,
            interp_base: 0,
            interp_path: None,
            brk: 0x2000,
        }
    }

    #[test]
    fn rsp_is_16_byte_aligned() {
        let mut m = Mem::new();
        // 参数长度故意取奇数，试图把对齐搞坏
        let rsp = setup(
            &mut m,
            &loaded(),
            &[b"prog".to_vec(), b"a".to_vec(), b"bbb".to_vec()],
            &[b"PATH=/bin".to_vec(), b"X=y".to_vec()],
        );
        assert_eq!(rsp % 16, 0, "rsp={rsp:#x}");
    }

    #[test]
    fn argc_argv_envp_layout_matches_abi() {
        let mut m = Mem::new();
        let argv = vec![b"/bin/echo".to_vec(), b"hi".to_vec()];
        let envp = vec![b"HOME=/root".to_vec()];
        let rsp = setup(&mut m, &loaded(), &argv, &envp);

        assert_eq!(m.read_u64(rsp).unwrap(), 2, "argc");
        let a0 = m.read_u64(rsp + 8).unwrap();
        let a1 = m.read_u64(rsp + 16).unwrap();
        assert_eq!(m.read_cstr(a0, 64).unwrap(), b"/bin/echo");
        assert_eq!(m.read_cstr(a1, 64).unwrap(), b"hi");
        assert_eq!(m.read_u64(rsp + 24).unwrap(), 0, "argv 以 NULL 收尾");
        let e0 = m.read_u64(rsp + 32).unwrap();
        assert_eq!(m.read_cstr(e0, 64).unwrap(), b"HOME=/root");
        assert_eq!(m.read_u64(rsp + 40).unwrap(), 0, "envp 以 NULL 收尾");
    }

    #[test]
    fn auxv_is_present_and_terminated() {
        let mut m = Mem::new();
        let l = loaded();
        let rsp = setup(&mut m, &l, &[b"p".to_vec()], &[]);
        // argc + argv[1] + NULL + envp NULL = 4 个 word
        let mut p = rsp + 4 * 8;
        let mut seen = std::collections::HashMap::new();
        loop {
            let t = m.read_u64(p).unwrap();
            let v = m.read_u64(p + 8).unwrap();
            if t == AT_NULL {
                break;
            }
            seen.insert(t, v);
            p += 16;
            assert!(p < STACK_TOP, "auxv 没有 AT_NULL 收尾");
        }
        assert_eq!(seen.get(&AT_PHDR), Some(&l.phdr_addr));
        assert_eq!(seen.get(&AT_PHENT), Some(&56));
        assert_eq!(seen.get(&AT_PHNUM), Some(&9));
        assert_eq!(seen.get(&AT_PAGESZ), Some(&4096));
        assert_eq!(seen.get(&AT_ENTRY), Some(&l.prog_entry));
        assert_eq!(seen.get(&AT_HWCAP), Some(&HWCAP));
        // AT_RANDOM 指向 16 字节可读区
        let r = *seen.get(&AT_RANDOM).unwrap();
        assert!(m.read(r, &mut [0u8; 16]).is_ok());
        // AT_PLATFORM 指向 "x86_64"
        assert_eq!(m.read_cstr(*seen.get(&AT_PLATFORM).unwrap(), 16).unwrap(), b"x86_64");
    }

    #[test]
    fn at_random_differs_between_runs() {
        let a = random_bytes();
        let b = random_bytes();
        assert_ne!(a, b, "AT_RANDOM 每次运行必须不同");
    }

    #[test]
    fn stack_is_readable_and_writable_below_rsp() {
        let mut m = Mem::new();
        let rsp = setup(&mut m, &loaded(), &[b"p".to_vec()], &[]);
        // guest 的 red zone 与后续 push 都在 rsp 下方
        assert!(m.write_u64(rsp - 128, 0x1234).is_ok());
        assert_eq!(m.read_u64(rsp - 128).unwrap(), 0x1234);
    }
}
