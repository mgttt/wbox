//! wbox 统一错误类型。
//!
//! 按 SPEC 约定映射退出码：
//!   1 = 参数错误
//!   2 = AppContainer profile 错误
//!   3 = Job Object 错误
//!   4 = 进程创建错误
//!   5 = OCI registry / 镜像拉取错误（网络、认证、digest 校验等）
//! 子进程自身退出码由 main 原样转发，不经过本类型。

use std::fmt;

/// 错误类别，同时决定进程退出码。
// 非 Windows 构建只使用 Args/Registry，其余变体由 Windows 专属模块使用。
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrKind {
    /// 命令行参数错误（退出码 1）
    Args,
    /// AppContainer profile 创建/派生失败（退出码 2）
    Profile,
    /// Job Object 创建/配置/分配失败（退出码 3）
    Job,
    /// 子进程创建失败（退出码 4）
    Spawn,
    /// OCI registry 网络 / 协议 / digest 校验错误（退出码 5）
    Registry,
}

impl ErrKind {
    /// 对应的进程退出码。
    pub fn exit_code(self) -> u32 {
        match self {
            ErrKind::Args => 1,
            ErrKind::Profile => 2,
            ErrKind::Job => 3,
            ErrKind::Spawn => 4,
            ErrKind::Registry => 5,
        }
    }

    /// 人类可读的类别名，用于错误输出前缀。
    pub fn label(self) -> &'static str {
        match self {
            ErrKind::Args => "参数错误",
            ErrKind::Profile => "AppContainer profile 错误",
            ErrKind::Job => "Job Object 错误",
            ErrKind::Spawn => "进程创建错误",
            ErrKind::Registry => "Registry/镜像错误",
        }
    }
}

/// 带退出码语义的 wbox 错误，内部用 [`crate::fault::Error`] 携带上下文链。
#[derive(Debug)]
pub struct WboxError {
    kind: ErrKind,
    inner: crate::fault::Error,
}

impl WboxError {
    /// 构造指定类别的错误。
    pub fn new(kind: ErrKind, inner: crate::fault::Error) -> Self {
        Self { kind, inner }
    }

    /// 便捷构造：参数错误。
    pub fn args(msg: impl Into<String>) -> Self {
        Self::msg(ErrKind::Args, msg)
    }

    /// 「该选项只在 Linux 宿主可用」的统一出口。
    ///
    /// 六个 Linux 专属选项（-v/-p/--user/--cap-*/--seccomp-deny/--health-cmd）
    /// 各写过一遍同形状的检查：**配置了且宿主不是 Linux 就带理由报错**。收敛到
    /// 这里保证两件事一致：报错的措辞结构（哪个选项、为何做不到）与"没配置就
    /// 一律放行"的判定。`configured=false` 恒 Ok——静默忽略只发生在配置了却
    /// 不生效的情况，那才是要防的。
    pub fn require_linux(configured: bool, flag: &str, why: &str) -> crate::error::Result<()> {
        if !configured || cfg!(target_os = "linux") {
            return Ok(());
        }
        Err(Self::args(format!(
            "{} 目前只在 Linux 宿主可用：{}",
            flag, why
        )))
    }

    /// 便捷构造：AppContainer profile 错误。
    // 仅 Windows 专属模块与测试使用。
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    pub fn profile(msg: impl Into<String>) -> Self {
        Self::msg(ErrKind::Profile, msg)
    }

    /// 便捷构造：Job Object 错误。
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    pub fn job(msg: impl Into<String>) -> Self {
        Self::msg(ErrKind::Job, msg)
    }

    /// 便捷构造：进程创建错误。
    pub fn spawn(msg: impl Into<String>) -> Self {
        Self::msg(ErrKind::Spawn, msg)
    }

    /// 便捷构造：registry/镜像错误。
    pub fn registry(msg: impl Into<String>) -> Self {
        Self::msg(ErrKind::Registry, msg)
    }

    /// 内部：字符串消息版构造（便捷构造共用）。
    fn msg(kind: ErrKind, msg: impl Into<String>) -> Self {
        Self::new(kind, crate::fail!(msg.into()))
    }

    /// 对应的进程退出码。
    pub fn exit_code(&self) -> u32 {
        self.kind.exit_code()
    }

    /// 错误类别（测试与审计用；非 test 构建暂无调用方）。
    #[allow(dead_code)]
    pub fn kind(&self) -> ErrKind {
        self.kind
    }
}

impl fmt::Display for WboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {:#}", self.kind.label(), self.inner)
    }
}

impl std::error::Error for WboxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.source()
    }
}

/// wbox 统一 Result 别名。
pub type Result<T> = std::result::Result<T, WboxError>;

/// 把 crate::fault::Error 包装成指定类别的 WboxError 的辅助 trait，
/// 用法：`foo().ctx(ErrKind::Job)?`。
pub trait KindExt<T> {
    fn ctx(self, kind: ErrKind) -> Result<T>;
}

impl<T> KindExt<T> for crate::fault::Result<T> {
    fn ctx(self, kind: ErrKind) -> Result<T> {
        self.map_err(|e| WboxError::new(kind, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_mapping_is_spec_fixed() {
        // SPEC §2：1=参数 2=profile 3=job 4=spawn 5=registry。
        // 这张映射表是 main.rs 错误到退出码的唯一出口，不得漂移。
        assert_eq!(ErrKind::Args.exit_code(), 1);
        assert_eq!(ErrKind::Profile.exit_code(), 2);
        assert_eq!(ErrKind::Job.exit_code(), 3);
        assert_eq!(ErrKind::Spawn.exit_code(), 4);
        assert_eq!(ErrKind::Registry.exit_code(), 5);
    }

    #[test]
    fn convenience_constructors_carry_kind_and_message() {
        let cases: [(WboxError, ErrKind); 5] = [
            (WboxError::args("a"), ErrKind::Args),
            (WboxError::profile("p"), ErrKind::Profile),
            (WboxError::job("j"), ErrKind::Job),
            (WboxError::spawn("s"), ErrKind::Spawn),
            (WboxError::registry("r"), ErrKind::Registry),
        ];
        for (e, kind) in cases {
            assert_eq!(e.kind(), kind);
            assert_eq!(e.exit_code(), kind.exit_code());
            // Display 含类别标签与消息
            let s = format!("{}", e);
            assert!(s.contains(kind.label()), "{}", s);
        }
    }

    #[test]
    fn ctx_wraps_fault_error_with_kind() {
        let r: crate::fault::Result<()> = Err(crate::fail!("底层原因"));
        let e = r.ctx(ErrKind::Job).unwrap_err();
        assert_eq!(e.kind(), ErrKind::Job);
        assert_eq!(e.exit_code(), 3);
        assert!(format!("{}", e).contains("底层原因"));
    }

    #[test]
    fn label_strings_are_nonempty_and_unique() {
        let labels = [
            ErrKind::Args.label(),
            ErrKind::Profile.label(),
            ErrKind::Job.label(),
            ErrKind::Spawn.label(),
            ErrKind::Registry.label(),
        ];
        for l in labels {
            assert!(!l.is_empty());
        }
        let mut uniq = labels.to_vec();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), labels.len());
    }
}
