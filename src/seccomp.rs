//! `--seccomp-deny`：按 syscall 拦截（`PRD.md` F9.9）。
//!
//! # 为什么是**拒绝名单**而不是 docker 的允许名单
//!
//! 说清楚这一点比实现本身更重要，否则用户会以为拿到了 docker 级别的边界。
//!
//! docker 用的是**允许名单**：默认放行一份精选的约三百个 syscall，其余一律
//! 拒绝。那份名单是它多年生产经验的产物；照抄一份自己没有同等验证的名单，
//! 会造出"看着像 docker、行为不一样"的坑——一个漏掉的 syscall 就是一类程序
//! 起不来，而且往往在很深的地方才炸。
//!
//! 拒绝名单的**代价必须直说**：它不是完备边界。没写进名单的 syscall 一律放行，
//! 内核新增的 syscall 也自动放行。它能兑现的是"这几个我明确不想让 guest 调的
//! 调不了"，这在运维上恰恰是最常见的诉求（挡掉 `ptrace`、`mount`、`keyctl`），
//! 且每一条都能验证。它不能兑现"未知的一律挡住"。
//!
//! # 与 `--cap-drop` 的关系
//!
//! 两者互补，都不可替代对方。capability 管的是"有没有权限做某类事"，seccomp
//! 管的是"这个入口能不能进"。有些 syscall 不需要任何 capability（`ptrace`
//! 同 uid 的进程就是），只能靠 seccomp 拦。

// `libc::SYS_*` 的类型是 `c_long`，在 64 位上恰好等于 i64、在 32 位上是 i32。
// 我们内部一律用 i64 存 syscall 号，故这些 `as i64` 在本机看着多余，换个架构
// 就是必需的。删掉它们只会让 32 位构建挂掉，所以按住这条 lint 而不是照做。
#![allow(clippy::unnecessary_cast)]

use crate::error::{Result, WboxError};

/// 精选的安全相关 syscall 名表。
///
/// **刻意不求全**：这里只收操作上真正会被点名拦截的那些。名字查不到时还可以
/// 直接写号（`--seccomp-deny 101`），所以这张表短并不构成能力缺口——它只是
/// 让常见写法有个可读的名字，而号是逃生口。
///
/// 号一律取自目标架构的 `libc::SYS_*`，不写死 x86-64 数字。catalog 只在已经有
/// `AUDIT_ARCH` 门禁的 ISA 上提供；某个 ABI 没有的 syscall 名不会伪造或映射到
/// 另一个入口（例如 AArch64 只有 `mknodat`，没有 legacy `mknod`）。
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub const SYSCALL_NAMES: &[(&str, libc::c_long)] = &[
    ("ptrace", libc::SYS_ptrace),
    ("mount", libc::SYS_mount),
    ("umount2", libc::SYS_umount2),
    ("pivot_root", libc::SYS_pivot_root),
    ("chroot", libc::SYS_chroot),
    ("kexec_load", libc::SYS_kexec_load),
    ("init_module", libc::SYS_init_module),
    ("finit_module", libc::SYS_finit_module),
    ("delete_module", libc::SYS_delete_module),
    ("keyctl", libc::SYS_keyctl),
    ("add_key", libc::SYS_add_key),
    ("request_key", libc::SYS_request_key),
    ("bpf", libc::SYS_bpf),
    ("perf_event_open", libc::SYS_perf_event_open),
    ("reboot", libc::SYS_reboot),
    ("swapon", libc::SYS_swapon),
    ("swapoff", libc::SYS_swapoff),
    ("setns", libc::SYS_setns),
    ("unshare", libc::SYS_unshare),
    ("clone", libc::SYS_clone),
    ("socket", libc::SYS_socket),
    ("connect", libc::SYS_connect),
    ("execve", libc::SYS_execve),
    ("execveat", libc::SYS_execveat),
    ("acct", libc::SYS_acct),
    ("settimeofday", libc::SYS_settimeofday),
    ("clock_settime", libc::SYS_clock_settime),
    #[cfg(target_arch = "x86_64")]
    ("mknod", libc::SYS_mknod),
    ("mknodat", libc::SYS_mknodat),
    ("quotactl", libc::SYS_quotactl),
    ("syslog", libc::SYS_syslog),
    #[cfg(target_arch = "x86_64")]
    ("uselib", libc::SYS_uselib),
    ("personality", libc::SYS_personality),
    ("process_vm_readv", libc::SYS_process_vm_readv),
    ("process_vm_writev", libc::SYS_process_vm_writev),
];

/// 非 Linux 或尚未适配 seccomp AUDIT_ARCH 的构建使用空表，只保留裸号解析。
#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
pub const SYSCALL_NAMES: &[(&str, i64)] = &[];

/// 生效的 seccomp 策略。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeccompPolicy {
    /// 要拒绝的 syscall 号，已排序去重。空 = 不装过滤器。
    denied: Vec<i64>,
}

/// 解析一条 `--seccomp-deny` 取值：名字或裸号，都可用逗号分隔多个。
fn parse_one(spec: &str) -> Result<i64> {
    let s = spec.trim();
    if s.is_empty() {
        return Err(WboxError::args("--seccomp-deny 取值为空"));
    }
    // 裸号是逃生口：名表刻意不求全，写不出名字时可以直接写号。
    if let Ok(n) = s.parse::<i64>() {
        if n < 0 {
            return Err(WboxError::args(format!(
                "--seccomp-deny '{}' 无效：syscall 号不能为负",
                s
            )));
        }
        return Ok(n);
    }
    let lower = s.to_ascii_lowercase();
    match SYSCALL_NAMES.iter().find(|(n, _)| *n == lower) {
        Some((_, nr)) => Ok(*nr as i64),
        None => Err(WboxError::args(format!(
            "未知 syscall '{}'：本表只收常被点名拦截的那些（{}），\
             不在表内可直接写号，例如 `--seccomp-deny 101`",
            s,
            SYSCALL_NAMES
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join(",")
        ))),
    }
}

impl SeccompPolicy {
    /// 由若干 `--seccomp-deny` 取值（每个可含逗号分隔的多项）求解。
    pub fn resolve(specs: &[String]) -> Result<Self> {
        let mut denied = Vec::new();
        for spec in specs {
            for part in spec.split(',') {
                denied.push(parse_one(part)?);
            }
        }
        denied.sort_unstable();
        denied.dedup();
        Ok(Self { denied })
    }

    /// 没有任何拒绝项：落地层整段跳过，不装过滤器。
    pub fn is_default(&self) -> bool {
        self.denied.is_empty()
    }

    /// 被拒绝的 syscall 号（已排序去重）。同模块的 `build_filter` 直接读字段，
    /// 故这个访问器只有测试在用。
    #[cfg(test)]
    pub fn denied(&self) -> &[i64] {
        &self.denied
    }

    /// 供 `-V` 打印。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn summary(&self) -> String {
        if self.is_default() {
            return "未设置（不装过滤器）".to_string();
        }
        let names: Vec<String> = self
            .denied
            .iter()
            .map(|nr| {
                SYSCALL_NAMES
                    .iter()
                    .find(|(_, n)| *n as i64 == *nr)
                    .map(|(name, _)| (*name).to_string())
                    .unwrap_or_else(|| format!("#{}", nr))
            })
            .collect();
        format!("拒绝 {}（返回 EPERM）", names.join(","))
    }
}

/// 拒绝那些**会让容器根本起不来**的配置。
///
/// 过滤器是在 `execve` guest 之**前**装上的（必须如此：装晚了 guest 已经在跑，
/// 拦不住它的第一条 syscall）。于是拒绝 `execve`/`execveat` 就等于拒绝启动
/// guest 自己，而且过滤器分不出"启动用的那次 exec"和"之后 guest 自己发起的
/// exec"，所以这个配置没有任何可用形态。
///
/// 不拦的话用户只会看到一句 "Operation not permitted"，完全看不出是自己的
/// `--seccomp-deny` 造成的。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn reject_self_defeating(policy: &SeccompPolicy) -> Result<()> {
    #[cfg(target_os = "linux")]
    for (name, nr) in [
        ("execve", libc::SYS_execve as i64),
        ("execveat", libc::SYS_execveat as i64),
    ] {
        if policy.denied.contains(&nr) {
            return Err(WboxError::args(format!(
                "--seccomp-deny {}：过滤器在启动 guest 之前装上，拒绝 {} 等于拒绝启动容器本身；\
                 且过滤器分不出启动用的那次 exec 与之后 guest 自己发起的 exec，故这个配置无可用形态",
                name, name
            )));
        }
    }
    let _ = policy;
    Ok(())
}

/// `--seccomp-deny` 在**当前宿主**是否可用。
pub fn reject_if_unsupported(policy: &SeccompPolicy) -> Result<()> {
    WboxError::require_linux(
        !policy.is_default(),
        "--seccomp-deny",
        "seccomp-bpf 是 Linux 的机制，Windows 侧没有等价的 syscall 级过滤器",
    )
}

// ---------------------------------------------------------------------------
// BPF 过滤器构造（Linux）
// ---------------------------------------------------------------------------

/// `seccomp_data.arch` 的期望值。**必须校验**：同一个 x86-64 内核也接受 x32
/// ABI 的调用，两套 ABI 的 syscall 号不同；不比对 arch 就会出现"按 x86-64 的
/// 号拦截，攻击者换 x32 的号绕过"。架构不匹配一律 kill，不是放行。
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const AUDIT_ARCH: u32 = 0xc000_003e; // AUDIT_ARCH_X86_64
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const AUDIT_ARCH: u32 = 0xc000_00b7; // AUDIT_ARCH_AARCH64

/// `seccomp_data` 里 `nr` 与 `arch` 的字节偏移（`struct seccomp_data` 前两个
/// 字段，各 4 字节）。
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
const OFF_NR: u32 = 0;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
const OFF_ARCH: u32 = 4;

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn stmt(code: u32, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code: code as u16,
        jt: 0,
        jf: 0,
        k,
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn jump(code: u32, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter {
        code: code as u16,
        jt,
        jf,
        k,
    }
}

/// 构造过滤器程序。纯函数，可单测——seccomp 一旦装上就不可撤销，
/// 靠"跑一遍看看"来调试的代价太高，指令序列必须能离线断言。
///
/// 程序形状：**每条拒绝项后面紧跟一个 `ret ERRNO`**，所以跳转距离恒为 0/1。
///
/// ```text
///   0: ld  [arch]
///   1: jeq AUDIT_ARCH ? +1 : fallthrough
///   2: ret KILL_PROCESS          // 架构不符
///   3: ld  [nr]
///   4: jeq <denied_0> ? fallthrough : +1
///   5: ret ERRNO(EPERM)
///   6: jeq <denied_1> ? fallthrough : +1
///   7: ret ERRNO(EPERM)
///   ...
///   4+2n: ret ALLOW
/// ```
///
/// # 为什么不用"命中就跳到末尾那一个 ERRNO"
///
/// 那种形状每条拒绝项只要一条指令，看着更省。但 BPF 的跳转偏移是 **`u8`**，
/// 而偏移随拒绝项数量线性增长：第 256 项时 `n - i` 算出 256，转成 `u8`
/// 溢出成 **0**——"跳 0 条"就是**继续往下走**，一路落到 `ret ALLOW`。
/// 也就是说**拒绝项一多，过滤器会静默地放行**（fail-open）。
///
/// 这一类错误最坏的地方是它不报错：策略照常装上，`wbox` 照常启动，
/// 只是安全边界没了。所以这里换成恒定 0/1 偏移的形状——**结构上不可能溢出**，
/// 而不是"加一条上限检查然后祈祷没人超"。代价是每条拒绝项多一条指令。
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub fn build_filter(policy: &SeccompPolicy) -> Vec<libc::sock_filter> {
    let n = policy.denied.len();
    let mut prog = Vec::with_capacity(2 * n + 5);
    prog.push(stmt(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, OFF_ARCH));
    prog.push(jump(
        libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
        AUDIT_ARCH,
        1,
        0,
    ));
    prog.push(stmt(
        libc::BPF_RET | libc::BPF_K,
        libc::SECCOMP_RET_KILL_PROCESS,
    ));
    prog.push(stmt(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, OFF_NR));
    let deny = stmt(
        libc::BPF_RET | libc::BPF_K,
        libc::SECCOMP_RET_ERRNO | (libc::EPERM as u32 & 0x0000_ffff),
    );
    for nr in policy.denied.iter() {
        // 相等则落到下一条（jt=0），即紧跟其后的 ret ERRNO；
        // 不等则跳过那一条（jf=1），继续比下一个。两个偏移都是常量。
        prog.push(jump(
            libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
            *nr as u32,
            0,
            1,
        ));
        prog.push(deny);
    }
    prog.push(stmt(libc::BPF_RET | libc::BPF_K, libc::SECCOMP_RET_ALLOW));
    prog
}

/// 装载过滤器。
///
/// # Safety
/// 仅在 fork 后的子进程中调用；只做裸 syscall。
///
/// 装载前必须置 `PR_SET_NO_NEW_PRIVS`：没有它，内核要求调用者持有
/// `CAP_SYS_ADMIN` 才允许装过滤器，而我们恰恰可能刚被 `--cap-drop` 削掉。
/// 它本身也是必要的——否则 setuid 程序可以在 execve 时提权绕过意图。
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub unsafe fn apply(prog: &[libc::sock_filter]) -> std::io::Result<()> {
    if prog.is_empty() {
        return Ok(());
    }
    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let fprog = libc::sock_fprog {
        len: prog.len() as u16,
        filter: prog.as_ptr() as *mut libc::sock_filter,
    };
    if libc::prctl(
        libc::PR_SET_SECCOMP,
        libc::SECCOMP_MODE_FILTER as libc::c_ulong,
        &fprog as *const _,
        0,
        0,
    ) != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// 未适配的架构：不能装一个号可能对不上的过滤器，宁可明确拒绝。
/// 静默不装是最坏的结果——用户以为拦住了。
#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
pub fn reject_unsupported_arch(policy: &SeccompPolicy) -> Result<()> {
    if policy.is_default() {
        return Ok(());
    }
    Err(WboxError::args(
        "--seccomp-deny 尚未适配本机架构：seccomp 过滤器要比对 AUDIT_ARCH，\
         适配前装载过滤器可能拦错 syscall，故明确拒绝而不是静默不装",
    ))
}

#[cfg(not(all(
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
)))]
pub fn reject_unsupported_arch(_policy: &SeccompPolicy) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_inert() {
        let p = SeccompPolicy::resolve(&[]).unwrap();
        assert!(p.is_default());
        assert!(p.summary().contains("不装过滤器"));
        assert!(reject_if_unsupported(&p).is_ok(), "默认策略任何宿主都放行");
    }

    #[test]
    fn accepts_raw_numbers_as_escape_hatch() {
        let p = SeccompPolicy::resolve(&["101".to_string()]).unwrap();
        assert_eq!(p.denied(), &[101]);
        assert!(SeccompPolicy::resolve(&["-1".to_string()]).is_err());
        assert!(SeccompPolicy::resolve(&["".to_string()]).is_err());
    }

    /// 拼错的名字必须报错，且要告诉用户逃生口在哪——否则遇到表外 syscall
    /// 的人只会以为 wbox 不支持。
    #[test]
    fn unknown_name_points_at_the_number_escape_hatch() {
        let e = SeccompPolicy::resolve(&["ptraze".to_string()]).unwrap_err();
        let m = format!("{}", e);
        assert!(m.contains("未知 syscall"), "{}", m);
        assert!(m.contains("直接写号"), "要指出逃生口：{}", m);
    }

    /// 拒绝 execve 等于拒绝启动容器自己。不拦的话用户只看到一句
    /// "Operation not permitted"，看不出是自己配的。
    #[cfg(target_os = "linux")]
    #[test]
    fn self_defeating_execve_denial_is_rejected_with_reason() {
        for name in ["execve", "execveat"] {
            let p = SeccompPolicy::resolve(&[name.to_string()]).unwrap();
            let m = format!("{}", reject_self_defeating(&p).unwrap_err());
            assert!(m.contains(name), "{}", m);
            assert!(m.contains("启动容器本身"), "要说清后果：{}", m);
        }
        let ok = SeccompPolicy::resolve(&["ptrace".to_string()]).unwrap();
        assert!(reject_self_defeating(&ok).is_ok());
        assert!(reject_self_defeating(&SeccompPolicy::default()).is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolves_names_and_dedups() {
        let p =
            SeccompPolicy::resolve(&["ptrace,mount".to_string(), "ptrace".to_string()]).unwrap();
        assert_eq!(p.denied().len(), 2, "重复项应去重：{:?}", p.denied());
        assert!(p.denied().contains(&(libc::SYS_ptrace as i64)));
        assert!(p.denied().contains(&(libc::SYS_mount as i64)));
        assert!(p.summary().contains("ptrace"));
        assert!(p.summary().contains("EPERM"));
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn x86_64_catalog_keeps_legacy_and_at_mknod_entries() {
        for name in ["mknod", "mknodat", "uselib"] {
            assert!(
                SeccompPolicy::resolve(&[name.to_owned()]).is_ok(),
                "x86-64 catalog should contain {name}"
            );
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    #[test]
    fn aarch64_catalog_exposes_only_real_mknod_entry() {
        let policy = SeccompPolicy::resolve(&["mknodat".to_owned()]).unwrap();
        assert_eq!(policy.denied(), &[libc::SYS_mknodat as i64]);
        for absent in ["mknod", "uselib"] {
            let error = SeccompPolicy::resolve(&[absent.to_owned()]).unwrap_err();
            assert!(format!("{error}").contains("未知 syscall"));
        }
    }

    /// seccomp 装上就撤不掉，靠"跑一遍看看"调试代价太高：指令序列必须能离线
    /// 断言。这条钉死跳转偏移——算错的话拒绝项会落到 ALLOW 上，
    /// 表现为"配置了却没拦住"，是最危险的失败形态。
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn filter_program_shape_and_jump_offsets() {
        let p = SeccompPolicy::resolve(&["ptrace,mount,chroot".to_string()]).unwrap();
        let prog = build_filter(&p);
        let n = p.denied().len();
        assert_eq!(n, 3);
        assert_eq!(prog.len(), 2 * n + 5);

        // 头部：先比对 arch，不符则 kill（不是放行）
        assert_eq!(prog[1].k, AUDIT_ARCH);
        assert_eq!(prog[2].k, libc::SECCOMP_RET_KILL_PROCESS);

        // 每条 jeq 命中就落到紧跟其后的 ERRNO；不命中跳过它。
        for i in 0..n {
            let at = 4 + 2 * i;
            assert_eq!(prog[at].jt, 0, "命中必须落到下一条（ret ERRNO）");
            assert_eq!(prog[at].jf, 1, "不命中必须跳过那一条 ret");
            assert_eq!(
                prog[at + 1].k,
                libc::SECCOMP_RET_ERRNO | (libc::EPERM as u32),
                "第 {i} 条拒绝项后面必须紧跟 ret ERRNO"
            );
        }

        // 尾部：比完所有拒绝项才 ALLOW
        assert_eq!(prog[prog.len() - 1].k, libc::SECCOMP_RET_ALLOW);
    }

    /// **fail-open 回归**：BPF 的跳转偏移是 `u8`。旧实现让"命中就跳到末尾
    /// 那一个 ERRNO"，偏移随拒绝项数量线性增长，第 256 项时溢出成 0——
    /// "跳 0 条"就是继续往下走，一路落到 `ret ALLOW`，**过滤器静默放行**。
    ///
    /// 这条用 300 条拒绝项走一遍，断言每条命中路径都真的到达 ERRNO、
    /// 且没有任何一条能走到 ALLOW。
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn large_policy_does_not_fail_open() {
        let mut pol = SeccompPolicy::default();
        // 用裸号构造，避开"名字表里没这么多"的问题。
        for nr in 1000..1300i64 {
            pol.denied.push(nr);
        }
        let n = pol.denied.len();
        assert_eq!(n, 300, "夹具本身要真的超过 255");
        let prog = build_filter(&pol);

        let allow_idx = prog.len() - 1;
        assert_eq!(prog[allow_idx].k, libc::SECCOMP_RET_ALLOW);

        for i in 0..n {
            let at = 4 + 2 * i;
            // 命中：落到 at+1，必须是 ERRNO，绝不能是 ALLOW。
            let hit = at + 1 + prog[at].jt as usize;
            assert_ne!(
                hit, allow_idx,
                "第 {i} 条拒绝项命中后走到了 ALLOW —— fail-open"
            );
            assert_eq!(
                prog[hit].k,
                libc::SECCOMP_RET_ERRNO | (libc::EPERM as u32),
                "第 {i} 条拒绝项命中后没有落在 ERRNO 上"
            );
        }

        // 指令数不得超过内核上限，否则 seccomp(2) 直接 EINVAL。
        assert!(
            prog.len() <= 4096,
            "程序 {} 条，超过 BPF_MAXINSNS",
            prog.len()
        );
    }

    /// 空策略也要能构造出合法程序（虽然落地层不会装它）。
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn empty_policy_still_builds_a_valid_allow_all_program() {
        let prog = build_filter(&SeccompPolicy::default());
        assert_eq!(prog.len(), 5);
        assert_eq!(prog[prog.len() - 1].k, libc::SECCOMP_RET_ALLOW);
    }
}
