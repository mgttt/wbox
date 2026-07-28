//! `--cap-add` / `--cap-drop`：Linux capability 面的裁剪（`PRD.md` F9.8）。
//!
//! # 这一格与 `--user` 的分工
//!
//! `--user`（F9.7）换的是**身份号**，`--cap-*` 改的才是**权限面**。二者常被
//! 混为一谈：rootless 下进程在自己的 user namespace 里始终持有全部 capability
//! （创建者身份使然），所以 `--user 1000` 并不降权。要真的收窄能力，只能动
//! capability 集合本身。
//!
//! # 这些 capability 管的是谁
//!
//! 只在**容器自己的命名空间内**有意义——userns 里的 `CAP_SYS_ADMIN` 换不来
//! 对宿主的任何权力。所以这不是"多加一道宿主防线"，而是限制 guest 在它自己
//! 的沙箱内还能做什么：挂载、raw socket、改自己的 uid、装载内核模块（在 ns
//! 内也会失败，但拿不到能力比拿到后被拒更早、更可预期）。
//!
//! # 默认值的取舍
//!
//! 默认**保留全部**，与本项目此前的行为一致，不因为引入这个开关就悄悄收紧
//! 已有用户的容器。docker 的默认是一份精选白名单；那份名单是它多年生产经验
//! 的产物，直接照抄却不具备同等验证，反而会造出"看着像 docker、行为不一样"
//! 的坑。要收紧的人写 `--cap-drop ALL` 即可，语义明确且可验证。

use crate::error::{Result, WboxError};

/// 已知 capability 名与其位号。取自 `include/uapi/linux/capability.h`，
/// 顺序即位号，故**只能往后追加，不能插入或重排**。
pub const CAP_NAMES: &[&str] = &[
    "CHOWN",              // 0
    "DAC_OVERRIDE",       // 1
    "DAC_READ_SEARCH",    // 2
    "FOWNER",             // 3
    "FSETID",             // 4
    "KILL",               // 5
    "SETGID",             // 6
    "SETUID",             // 7
    "SETPCAP",            // 8
    "LINUX_IMMUTABLE",    // 9
    "NET_BIND_SERVICE",   // 10
    "NET_BROADCAST",      // 11
    "NET_ADMIN",          // 12
    "NET_RAW",            // 13
    "IPC_LOCK",           // 14
    "IPC_OWNER",          // 15
    "SYS_MODULE",         // 16
    "SYS_RAWIO",          // 17
    "SYS_CHROOT",         // 18
    "SYS_PTRACE",         // 19
    "SYS_PACCT",          // 20
    "SYS_ADMIN",          // 21
    "SYS_BOOT",           // 22
    "SYS_NICE",           // 23
    "SYS_RESOURCE",       // 24
    "SYS_TIME",           // 25
    "SYS_TTY_CONFIG",     // 26
    "MKNOD",              // 27
    "LEASE",              // 28
    "AUDIT_WRITE",        // 29
    "AUDIT_CONTROL",      // 30
    "SETFCAP",            // 31
    "MAC_OVERRIDE",       // 32
    "MAC_ADMIN",          // 33
    "SYSLOG",             // 34
    "WAKE_ALARM",         // 35
    "BLOCK_SUSPEND",      // 36
    "AUDIT_READ",         // 37
    "PERFMON",            // 38
    "BPF",                // 39
    "CHECKPOINT_RESTORE", // 40
];

/// 本表覆盖到的最高位号。运行时内核可能认识更多（见 [`CapPolicy::retained`]
/// 的说明），故这个常数只用于**解析**，不用于枚举要丢弃的范围。
const KNOWN_LAST: u32 = CAP_NAMES.len() as u32 - 1;

/// 全部已知 capability 的位掩码。
const ALL_KNOWN: u64 = if KNOWN_LAST >= 63 {
    u64::MAX
} else {
    (1u64 << (KNOWN_LAST + 1)) - 1
};

/// `--cap-add` / `--cap-drop` 的单个取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapSelector {
    /// `ALL`
    All,
    /// 单个 capability 的位号
    One(u32),
}

/// 解析一个 capability 名。
///
/// 与 docker 一样接受带不带 `CAP_` 前缀、大小写不敏感，外加 `ALL`。
/// 认不出的名字**必须报错**：静默忽略一个拼错的 `--cap-drop` 会让用户以为
/// 已经收紧了，而实际上什么都没发生——这比不支持该选项危险得多。
pub fn parse_cap(spec: &str) -> Result<CapSelector> {
    let upper = spec.trim().to_ascii_uppercase();
    let bare = upper.strip_prefix("CAP_").unwrap_or(&upper);
    if bare == "ALL" {
        return Ok(CapSelector::All);
    }
    match CAP_NAMES.iter().position(|n| *n == bare) {
        Some(i) => Ok(CapSelector::One(i as u32)),
        None => Err(WboxError::args(format!(
            "未知 capability '{}'：可用名字见 `man 7 capabilities`（可写 CAP_NET_RAW 或 NET_RAW，\
             另支持 ALL）",
            spec
        ))),
    }
}

/// 最终生效的 capability 集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapPolicy {
    /// 保留位掩码；bit i = capability i 保留。
    ///
    /// 只覆盖**本表已知**的位。内核认识的位可能更多（新内核会加），那些位
    /// 的处置由落地层按 `/proc/sys/kernel/cap_last_cap` 决定：默认策略下一律
    /// 保留，`ALL` 被丢弃时一并丢弃。这样既不会因为编译期的表落后于内核而
    /// 漏掉新 capability，也不会凭空丢掉用户没要求丢的东西。
    retained: u64,
    /// 是否出现过 `--cap-drop ALL`。决定表外（未知）位号怎么处置。
    dropped_unknown: bool,
    /// 用户是否显式动过 capability。默认策略下落地层整段跳过，
    /// 一个 `prctl` 都不调——不为"什么都不改"付出行为变化的风险。
    touched: bool,
}

impl Default for CapPolicy {
    /// 默认保留全部：引入这个开关不改变既有容器的行为。
    fn default() -> Self {
        Self {
            retained: ALL_KNOWN,
            dropped_unknown: false,
            touched: false,
        }
    }
}

// 下面几个查询方法只有 Linux 后端（backend/linux_ns.rs）消费；**规则本身跨平台**，
// 故定义与单测都留在这里，与 Limits 的处理一致。
impl CapPolicy {
    /// 按 docker 的顺序求解：先 drop 后 add，故 `--cap-drop ALL --cap-add NET_RAW`
    /// 得到"只剩 NET_RAW"，与直觉一致。
    pub fn resolve(drops: &[CapSelector], adds: &[CapSelector]) -> Self {
        let mut p = Self::default();
        if drops.is_empty() && adds.is_empty() {
            return p;
        }
        p.touched = true;
        for d in drops {
            match d {
                CapSelector::All => {
                    p.retained = 0;
                    p.dropped_unknown = true;
                }
                CapSelector::One(i) => p.retained &= !(1u64 << i),
            }
        }
        for a in adds {
            match a {
                CapSelector::All => {
                    p.retained = ALL_KNOWN;
                    p.dropped_unknown = false;
                }
                CapSelector::One(i) => p.retained |= 1u64 << i,
            }
        }
        p
    }

    /// 用户没动过 capability：落地层应整段跳过。
    pub fn is_default(&self) -> bool {
        !self.touched
    }

    /// 位 `i` 是否保留。表外位号按 [`Self::dropped_unknown`] 处置。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn keeps(&self, i: u32) -> bool {
        if i > KNOWN_LAST {
            return !self.dropped_unknown;
        }
        self.retained & (1u64 << i) != 0
    }

    /// 保留位掩码（低 64 位）。落地层写 `capset` 时直接用。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn mask_low(&self) -> u32 {
        (self.retained & 0xffff_ffff) as u32
    }

    /// 保留位掩码（高 32 位）。表外位号在这一段里，故要按
    /// `dropped_unknown` 补齐——否则新内核的高位 capability 会被静默丢掉。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn mask_high(&self, last_cap: u32) -> u32 {
        let mut hi = (self.retained >> 32) as u32;
        if !self.dropped_unknown {
            for i in KNOWN_LAST + 1..=last_cap.min(63) {
                if i >= 32 {
                    hi |= 1u32 << (i - 32);
                }
            }
        }
        hi
    }

    /// 人类可读摘要，供 `-V` 打印。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn summary(&self) -> String {
        if self.is_default() {
            return "全部保留（默认）".to_string();
        }
        let kept: Vec<&str> = CAP_NAMES
            .iter()
            .enumerate()
            .filter(|(i, _)| self.keeps(*i as u32))
            .map(|(_, n)| *n)
            .collect();
        if kept.is_empty() {
            "全部丢弃".to_string()
        } else {
            format!("仅保留 {}", kept.join(","))
        }
    }
}

/// `--cap-add`/`--cap-drop` 在**当前宿主**是否可用。
///
/// 非 Linux 宿主必须明确报错：capability 是 Linux 概念，Windows 的
/// AppContainer 走的是 SID + capability SID 的另一套模型，两者不能互相翻译。
/// 静默忽略会让用户以为已经收紧了。
pub fn reject_if_unsupported(policy: &CapPolicy) -> Result<()> {
    WboxError::require_linux(
        !policy.is_default(),
        "--cap-add/--cap-drop",
        "capability 是 Linux 的概念，Windows 侧 AppContainer 用的是另一套 SID 模型，无法逐条对应",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_with_and_without_prefix() {
        assert_eq!(parse_cap("NET_RAW").unwrap(), CapSelector::One(13));
        assert_eq!(parse_cap("CAP_NET_RAW").unwrap(), CapSelector::One(13));
        assert_eq!(parse_cap("cap_net_raw").unwrap(), CapSelector::One(13));
        assert_eq!(parse_cap(" all ").unwrap(), CapSelector::All);
        assert_eq!(parse_cap("CHOWN").unwrap(), CapSelector::One(0));
    }

    /// 拼错的名字必须报错。静默忽略会让用户以为已经收紧了，
    /// 那比不支持这个选项更危险。
    #[test]
    fn rejects_unknown_name_with_actionable_message() {
        let e = parse_cap("NET_RAWW").unwrap_err();
        let m = format!("{}", e);
        assert!(m.contains("未知 capability"), "{}", m);
        assert!(m.contains("capabilities"), "要指出去哪查名字：{}", m);
        assert!(parse_cap("").is_err());
    }

    #[test]
    fn default_keeps_everything_and_is_marked_untouched() {
        let p = CapPolicy::default();
        assert!(p.is_default());
        for i in 0..=KNOWN_LAST {
            assert!(p.keeps(i), "默认应保留 {}", CAP_NAMES[i as usize]);
        }
        // 表外位号（新内核可能有）默认也保留
        assert!(p.keeps(62));
    }

    /// docker 的顺序是先 drop 后 add，故这一组应只剩 NET_RAW。
    #[test]
    fn drop_all_then_add_keeps_only_added() {
        let p = CapPolicy::resolve(&[CapSelector::All], &[CapSelector::One(13)]);
        assert!(!p.is_default());
        assert!(p.keeps(13));
        assert!(!p.keeps(21), "SYS_ADMIN 应已丢弃");
        assert!(!p.keeps(0));
        // ALL 被丢弃时，表外位号也一并丢弃
        assert!(!p.keeps(62));
        assert_eq!(p.mask_low(), 1 << 13);
        assert_eq!(p.mask_high(63), 0);
    }

    #[test]
    fn single_drop_leaves_the_rest() {
        let p = CapPolicy::resolve(&[CapSelector::One(21)], &[]);
        assert!(!p.keeps(21));
        assert!(p.keeps(13));
        assert!(p.keeps(0));
    }

    /// 编译期的名字表可能落后于内核。没写 `drop ALL` 时，表外位号必须保留，
    /// 否则升级内核会静默丢掉新 capability。
    #[test]
    fn unknown_bits_survive_unless_all_dropped() {
        let p = CapPolicy::resolve(&[CapSelector::One(0)], &[]);
        assert!(p.keeps(62), "没说丢 ALL 就不该丢表外位");
        let hi = p.mask_high(45);
        for i in KNOWN_LAST + 1..=45 {
            assert!(hi & (1u32 << (i - 32)) != 0, "位 {} 应保留", i);
        }
    }

    #[test]
    fn summary_is_readable() {
        assert!(CapPolicy::default().summary().contains("默认"));
        assert_eq!(
            CapPolicy::resolve(&[CapSelector::All], &[]).summary(),
            "全部丢弃"
        );
        let s = CapPolicy::resolve(&[CapSelector::All], &[CapSelector::One(13)]).summary();
        assert_eq!(s, "仅保留 NET_RAW");
    }

    #[test]
    fn rejected_off_linux_only_when_touched() {
        assert!(
            reject_if_unsupported(&CapPolicy::default()).is_ok(),
            "默认策略任何宿主都放行"
        );
        let p = CapPolicy::resolve(&[CapSelector::All], &[]);
        let r = reject_if_unsupported(&p);
        if cfg!(target_os = "linux") {
            assert!(r.is_ok());
        } else {
            let m = format!("{}", r.unwrap_err());
            assert!(m.contains("只在 Linux"), "{}", m);
            assert!(m.contains("AppContainer"), "要说清为什么做不到：{}", m);
        }
    }
}
