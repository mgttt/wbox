//! 带上下文链的通用错误类型 —— 取代 `anyhow`。
//!
//! wbox 用到的 `anyhow` 只有四样：一个能装下任意底层错误的 `Error`、
//! 一个往上加说明的 `.context(...)`、几个构造宏，以及 `{:#}` 打出"外层：
//! 内层：根因"的那种展示。这里就实现这四样。
//!
//! # 与 `anyhow` 的两处有意差异
//!
//! 1. **上下文链是 `Vec<String>`，不是嵌套的 trait object**。丢掉的是
//!    "逐层向下取回原始错误类型"的能力——wbox 从来没用过它（错误一旦被
//!    `WboxError` 收编，对外只剩类别与文案）。换来的是实现只有几十行。
//! 2. **`Error` 不实现 `std::error::Error`**。这不是偷懒：实现了它，
//!    下面那条"任意标准错误都能 `?` 进来"的 blanket `From` 就会与 std 的
//!    反身 `From<T> for T` 冲突。`anyhow` 出于同样的原因也没实现。
//!    需要向外暴露成标准错误的地方是 [`crate::error::WboxError`]，它实现了。

use std::fmt;

/// 带上下文链的错误。
///
/// `chain[0]` 是最外层（最后加上的说明），末项是最内层（根因）。
#[derive(Debug)]
pub struct Error {
    chain: Vec<String>,
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl Error {
    /// 由一条消息构造。
    pub fn msg(m: impl fmt::Display) -> Self {
        Self {
            chain: vec![m.to_string()],
            source: None,
        }
    }

    /// 在外面再套一层说明。
    pub fn context(mut self, c: impl fmt::Display) -> Self {
        self.chain.insert(0, c.to_string());
        self
    }

    /// 底层错误（若有）。`WboxError::source` 转发它。
    pub fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|e| e.as_ref() as &(dyn std::error::Error + 'static))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            // `{:#}`：整条链，从外到内。与 anyhow 的展示一致（它用 ": "，
            // 这里用中文冒号，与项目其它错误文案统一）。
            f.write_str(&self.chain.join("："))
        } else {
            f.write_str(self.chain.first().map(String::as_str).unwrap_or("未知错误"))
        }
    }
}

/// 任意标准错误都能 `?` 进来（`io::Error` 是主要来源）。
impl<E: std::error::Error + Send + Sync + 'static> From<E> for Error {
    fn from(e: E) -> Self {
        Self {
            chain: vec![e.to_string()],
            source: Some(Box::new(e)),
        }
    }
}

/// 与 `anyhow::Result` 同形（第二个类型参数带默认值，`Result<T>` 直接可用）。
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// 给 `Result` / `Option` 加上下文说明。
pub trait Context<T> {
    /// 加一条固定说明。
    fn context(self, c: impl fmt::Display) -> Result<T>;
    /// 加一条按需生成的说明（成功路径上不构造字符串）。
    fn with_context<C: fmt::Display, F: FnOnce() -> C>(self, f: F) -> Result<T>;
}

impl<T, E: Into<Error>> Context<T> for Result<T, E> {
    fn context(self, c: impl fmt::Display) -> Result<T> {
        self.map_err(|e| e.into().context(c))
    }

    fn with_context<C: fmt::Display, F: FnOnce() -> C>(self, f: F) -> Result<T> {
        self.map_err(|e| e.into().context(f()))
    }
}

impl<T> Context<T> for Option<T> {
    fn context(self, c: impl fmt::Display) -> Result<T> {
        self.ok_or_else(|| Error::msg(c))
    }

    fn with_context<C: fmt::Display, F: FnOnce() -> C>(self, f: F) -> Result<T> {
        self.ok_or_else(|| Error::msg(f()))
    }
}

/// 构造 [`Error`]，用法同 `anyhow::anyhow!`。
#[macro_export]
macro_rules! fail {
    ($fmt:literal $($arg:tt)*) => { $crate::fault::Error::msg(format!($fmt $($arg)*)) };
    ($e:expr) => { $crate::fault::Error::msg($e) };
}

/// 直接返回一个 [`Error`]，用法同 `anyhow::bail!`。
#[macro_export]
macro_rules! bail {
    ($($tt:tt)+) => { return Err($crate::fail!($($tt)+)) };
}

/// 条件不成立就返回错误，用法同 `anyhow::ensure!`。
#[macro_export]
macro_rules! ensure {
    ($cond:expr, $($tt:tt)+) => {
        if !($cond) { $crate::bail!($($tt)+); }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_chain_shows_outer_to_inner() {
        let e = Error::msg("根因").context("中层").context("外层");
        // `{}` 只出最外层，`{:#}` 出整条链——WboxError 用的是后者。
        assert_eq!(format!("{}", e), "外层");
        assert_eq!(format!("{:#}", e), "外层：中层：根因");
    }

    #[test]
    fn io_errors_convert_and_keep_source() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "没有这个文件");
        let e: Error = io.into();
        assert!(format!("{}", e).contains("没有这个文件"));
        assert!(e.source().is_some(), "底层错误要留得住");
    }

    #[test]
    fn context_on_result_and_option() {
        let r: std::result::Result<(), std::io::Error> =
            Err(std::io::Error::other("底层"));
        let e = Context::context(r, "读取失败").unwrap_err();
        assert_eq!(format!("{:#}", e), "读取失败：底层");

        let o: Option<u8> = None;
        let e = o.context("缺字段").unwrap_err();
        assert_eq!(format!("{}", e), "缺字段");

        // with_context 只在失败路径上求值。
        let ok: Option<u8> = Some(1);
        assert_eq!(ok.with_context(|| -> String { panic!("不该被求值") }).unwrap(), 1);
    }

    #[test]
    fn macros_build_errors() {
        fn f(n: u32) -> Result<u32> {
            ensure!(n > 0, "n 必须为正，收到 {n}");
            if n > 10 {
                bail!("n 太大：{}", n);
            }
            Ok(n)
        }
        assert_eq!(f(3).unwrap(), 3);
        assert_eq!(format!("{}", f(0).unwrap_err()), "n 必须为正，收到 0");
        assert_eq!(format!("{}", f(11).unwrap_err()), "n 太大：11");
        assert_eq!(format!("{}", fail!("裸消息")), "裸消息");
    }
}
