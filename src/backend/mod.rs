//! 后端抽象：把 `wbox run` 的执行目标（本地 Windows 程序 / OCI 镜像）
//! 分派到不同的运行时后端。
//!
//! - [`NativeBackend`]：包装现有 AppContainer + Job Object 隔离逻辑，
//!   直接运行 Windows 原生程序（v1 能力）。
//! - [`BlinkBackend`]：Linux 用户态模拟后端骨架。定位 `wbox-linux.exe`
//!   （blink 的 wbox 移植版，exe 同目录或 `WBOX_LINUX` 环境变量），
//!   构造 rootfs / `BLINK_PREFIX` 参数后，**仍经 NativeBackend 的
//!   AppContainer 拉起 wbox-linux.exe**（双层隔离：外层 AppContainer
//!   关住模拟器，模拟器再关住 guest Linux 进程）。
//!   blink 的 Win32 移植尚未完成，故找不到 exe 时给出明确错误。
//!
//! 该模块跨平台可编译（Windows 专属部分在 native.rs / blink.rs 内 cfg），
//! 使命令行合并、exe 定位等纯逻辑能在 Linux 沙箱单测。

mod blink;
#[cfg(windows)]
mod native;

pub use blink::BlinkBackend;
#[cfg(windows)]
pub use native::NativeBackend;

use crate::error::Result;
use std::path::PathBuf;

/// 资源限额（跨平台描述；Windows 侧映射到 JobLimits）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limits {
    /// 每进程内存上限（MB），0 = 不限
    pub memory_mb: u64,
    /// CPU 硬性百分比上限 1-100，0 = 不限
    pub cpu_pct: u32,
    /// 最大进程数，0 = 不限
    pub max_procs: u32,
}

/// 一次 `run` 的完整规格（后端无关）。
// 非 Windows 构建下 name/limits 等仅由 Windows 专属 spawn 读取。
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone)]
pub struct RunSpec {
    /// 容器名（AppContainer profile 名）
    pub name: String,
    /// 资源限额
    pub limits: Limits,
    /// 是否授予 INTERNET_CLIENT capability
    pub allow_network: bool,
    /// 退出后保留 AppContainer profile
    pub keep_profile: bool,
    /// 容器工作目录（原生模式）/ rootfs 目录（镜像模式）
    pub workdir: PathBuf,
    /// 最终命令行（镜像模式下已按 docker 规则合并 Entrypoint/Cmd）
    pub cmd: Vec<String>,
    /// 注入子进程的环境变量（镜像 config Env；原生模式为空）
    pub env: Vec<(String, String)>,
    /// 打印隔离配置摘要
    pub verbose: bool,
}

/// 后端 prepare 的产出：可直接 spawn 的执行计划。
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone)]
pub struct Prepared {
    /// 最终要执行的命令行（镜像模式下为 wbox-linux.exe + 参数）
    pub cmd: Vec<String>,
    /// 子进程工作目录
    pub workdir: PathBuf,
    /// 注入的环境变量
    pub env: Vec<(String, String)>,
}

/// 容器后端：准备执行计划 + 启动并等待。
pub trait Backend {
    /// 校验目标并构造执行计划（不启动进程）。
    fn prepare(&self, spec: &RunSpec) -> Result<Prepared>;
    /// 启动进程并等待退出，返回子进程退出码。
    fn spawn(&self, spec: &RunSpec, prepared: &Prepared) -> Result<u32>;
}

/// `run` 的执行目标判别结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunTarget {
    /// 本地 Windows 可执行路径（v1 行为）
    Native,
    /// OCI 镜像引用（已解析 + 已确认本地可用）
    Image(crate::oci::ImageRef),
}

/// 判别"镜像引用 vs 本地可执行路径"。
///
/// 规则（对齐任务约定）：位置参数能解析为 [`crate::oci::ImageRef`]
/// **且**（已在本地缓存中 或 用户显式给了 `--pull`）时视为镜像引用；
/// 否则回退为本地命令（保持 v1 `wbox run [opts] cmd.exe ...` 兼容）。
/// 这样 `cmd.exe`、`C:\app\tool.exe` 这类也能被 ImageRef 语法接受的
/// 字符串，只要没 pull 过、也没带 `--pull`，就不会被误判为镜像。
pub fn classify_target(
    positional: Option<&str>,
    pull: bool,
    is_pulled: impl Fn(&crate::oci::ImageRef) -> bool,
) -> Result<RunTarget> {
    let Some(s) = positional else {
        return Ok(RunTarget::Native);
    };
    if let Ok(iref) = crate::oci::ImageRef::parse(s, None) {
        if pull || is_pulled(&iref) {
            return Ok(RunTarget::Image(iref));
        }
    }
    Ok(RunTarget::Native)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never_pulled(_: &crate::oci::ImageRef) -> bool {
        false
    }

    #[test]
    fn no_positional_is_native() {
        assert_eq!(classify_target(None, false, never_pulled).unwrap(), RunTarget::Native);
    }

    #[test]
    fn pulled_image_ref_is_image() {
        let pulled = |r: &crate::oci::ImageRef| r.repo == "library/ubuntu";
        match classify_target(Some("ubuntu:24.04"), false, pulled).unwrap() {
            RunTarget::Image(r) => {
                assert_eq!(r.repo, "library/ubuntu");
                assert_eq!(r.reference, "24.04");
            }
            other => panic!("期望 Image，得到 {:?}", other),
        }
    }

    #[test]
    fn unpulled_image_ref_without_pull_flag_is_native() {
        // 能解析为 ImageRef 但未 pull 且无 --pull：回退本地命令（v1 兼容）
        assert_eq!(
            classify_target(Some("ubuntu:24.04"), false, never_pulled).unwrap(),
            RunTarget::Native
        );
    }

    #[test]
    fn pull_flag_promotes_unpulled_ref_to_image() {
        match classify_target(Some("alpine:3.20"), true, never_pulled).unwrap() {
            RunTarget::Image(r) => assert_eq!(r.repo, "library/alpine"),
            other => panic!("期望 Image，得到 {:?}", other),
        }
    }

    #[test]
    fn local_exe_path_stays_native() {
        // 本地程序名/路径即使语法上像镜像引用，未 pull 时也是本地命令
        for s in ["cmd.exe", "notepad.exe", r"C:\tools\app.exe", "./run.sh"] {
            assert_eq!(
                classify_target(Some(s), false, never_pulled).unwrap(),
                RunTarget::Native,
                "{}",
                s
            );
        }
    }
}
