//! 非 Linux 宿主上的 wine 空实现。
//!
//! Windows 宿主跑 PE 走的是原生 AppContainer 路径，根本不需要 wine；其它宿主
//! 尚无实现。保留一个同名、同签名的空函数，是为了让 `linux.rs` 的 `prepare`
//! 里**一个 `cfg` 都不用写**——否则调用点要么散落条件编译，要么被迫给一堆
//! 绑定挂 `#[allow(unused_mut)]`（那正是重构前的样子）。

use crate::error::Result;

/// 恒不包装：本宿主上不存在"用 wine 跑 PE"这条路径。
pub fn wrap_if_pe(
    _cmd: &mut Vec<String>,
    _env: &mut Vec<(String, String)>,
    _verbose: bool,
) -> Result<Option<Vec<(&'static str, String)>>> {
    Ok(None)
}

/// 恒为 false：本宿主上没有"PE 要不要转交 wine"这个问题。
///
/// Windows 宿主确实会跑 PE，但那是 `NativeBackend` 的原生路径，压根不经过
/// 这里；镜像模式的 PE 拦截也只对 Linux 宿主有意义。给出常量实现，调用方
/// 就不必为了一句判断而写 `cfg`。
pub fn is_pe(_path: &std::path::Path) -> bool {
    false
}
