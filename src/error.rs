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

/// 带退出码语义的 wbox 错误，内部用 anyhow 携带上下文链。
#[derive(Debug)]
pub struct WboxError {
    kind: ErrKind,
    inner: anyhow::Error,
}

impl WboxError {
    /// 构造指定类别的错误。
    pub fn new(kind: ErrKind, inner: anyhow::Error) -> Self {
        Self { kind, inner }
    }

    /// 便捷构造：参数错误。
    pub fn args(msg: impl Into<String>) -> Self {
        Self::new(ErrKind::Args, anyhow::anyhow!(msg.into()))
    }

    /// 便捷构造：registry/镜像错误。
    pub fn registry(msg: impl Into<String>) -> Self {
        Self::new(ErrKind::Registry, anyhow::anyhow!(msg.into()))
    }

    /// 对应的进程退出码。
    pub fn exit_code(&self) -> u32 {
        self.kind.exit_code()
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

/// 把 anyhow::Error 包装成指定类别的 WboxError 的辅助 trait，
/// 用法：`foo().ctx(ErrKind::Job)?`。
pub trait KindExt<T> {
    fn ctx(self, kind: ErrKind) -> Result<T>;
}

impl<T> KindExt<T> for anyhow::Result<T> {
    fn ctx(self, kind: ErrKind) -> Result<T> {
        self.map_err(|e| WboxError::new(kind, e))
    }
}
