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
pub mod env;
// native 的 prepare 纯逻辑跨平台可编译（spawn 链路内部 cfg），
// 使命令校验/环境构造可在 Linux 沙箱单测。
mod native;

pub use blink::BlinkBackend;
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
    /// `--env-pass-all`：继承完整宿主环境（默认仅白名单；
    /// 保留键 BLINK_*/WBOX_* 两路均不透传，见 env.rs）
    pub env_pass_all: bool,
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

// ---- 后端共享的 prepare 辅助（native / blink 公共逻辑下沉）----

/// 校验命令非空：两后端 prepare 的第一道闸门。
pub(crate) fn require_cmd(cmd: &[String]) -> Result<()> {
    if cmd.is_empty() {
        return Err(crate::error::WboxError::args(
            "缺少要执行的命令（镜像模式请合并 Entrypoint/Cmd，或在 `--` 后显式给出）",
        ));
    }
    Ok(())
}

/// verbose 输出的统一结构化形式：`wbox: <key> = <value>`。
/// 各后端/CLI 的 verbose 行统一经此输出，避免格式漂移。
pub(crate) fn verbose_kv(key: &str, value: impl std::fmt::Display) {
    println!("wbox: {} = {}", key, value);
}

/// 构造子进程显式环境（H2/H6 统一路径）：
/// 过滤 spec.env 的保留键（verbose 时报告丢弃清单），并入 wbox 强制项，
/// 最后按 pass_all 决定白名单/全量继承。两后端共用，保证策略单一出口。
pub(crate) fn build_sanitized_env(
    spec_env: &[(String, String)],
    forced: &[(String, String)],
    pass_all: bool,
    verbose: bool,
) -> Vec<(String, String)> {
    let (img_env, dropped) = env::sanitize_image_env(spec_env);
    if verbose && !dropped.is_empty() {
        println!(
            "wbox: 已丢弃环境变量中的保留键（隔离/凭证相关）：{}",
            dropped.join(", ")
        );
    }
    env::build_child_env(&img_env, forced, pass_all)
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

    // ---- classify_target 边界：ubuntu vs ./ubuntu vs ubuntu:latest vs 绝对路径 ----

    #[test]
    fn classify_bare_name_tagged_and_path_forms() {
        let pulled_ubuntu = |r: &crate::oci::ImageRef| r.repo == "library/ubuntu";
        // "ubuntu"（无 tag → latest）已缓存 → Image
        match classify_target(Some("ubuntu"), false, pulled_ubuntu).unwrap() {
            RunTarget::Image(r) => assert_eq!(r.reference, "latest"),
            other => panic!("期望 Image，得到 {:?}", other),
        }
        // "ubuntu:latest" 同样 → Image
        match classify_target(Some("ubuntu:latest"), false, pulled_ubuntu).unwrap() {
            RunTarget::Image(r) => assert_eq!(r.reference, "latest"),
            other => panic!("期望 Image，得到 {:?}", other),
        }
        // 未缓存时全部回退 Native
        for s in ["ubuntu", "ubuntu:latest", "./ubuntu", "/usr/bin/ubuntu", r"C:\ubuntu\app.exe"] {
            assert_eq!(
                classify_target(Some(s), false, never_pulled).unwrap(),
                RunTarget::Native,
                "未缓存的 {} 必须回退 Native",
                s
            );
        }
    }

    #[test]
    fn classify_relative_path_parsed_but_needs_pull() {
        // "./ubuntu" 语法上可被 ImageRef 接受（首段 "." 含点 → 视作 registry）；
        // 未缓存时仍回退 Native，只有显式 --pull 才提升为 Image（记录现状）。
        assert_eq!(
            classify_target(Some("./ubuntu"), false, never_pulled).unwrap(),
            RunTarget::Native
        );
        match classify_target(Some("./ubuntu"), true, never_pulled).unwrap() {
            RunTarget::Image(r) => {
                assert_eq!(r.registry, ".");
                assert_eq!(r.repo, "ubuntu");
            }
            other => panic!("期望 Image，得到 {:?}", other),
        }
    }

    #[test]
    fn classify_absolute_path_never_image_without_pull() {
        // 绝对路径未 pull 恒为 Native（v1 兼容的关键保证）
        for s in ["/usr/bin/foo", r"C:\x\y.exe", r"D:\tools"] {
            assert_eq!(
                classify_target(Some(s), false, never_pulled).unwrap(),
                RunTarget::Native,
                "{}",
                s
            );
        }
        // 记录现状：带 --pull 时 Windows 路径的盘符冒号被当作 tag 分隔符，
        // "C:\x\y.exe" 解析为 repo="C"、reference="\x\y.exe" → 被判为镜像。
        // （--pull 即用户显式声明目标是镜像，此行为可接受但值得注意。）
        match classify_target(Some(r"C:\x\y.exe"), true, never_pulled).unwrap() {
            RunTarget::Image(r) => assert_eq!(r.repo, "library/C"), // docker hub 补全照常
            other => panic!("期望 Image，得到 {:?}", other),
        }
    }

    #[test]
    fn classify_none_positional_with_pull_is_native() {
        // 无位置参数 + --pull：仍 Native（-- 后命令）
        assert_eq!(classify_target(None, true, never_pulled).unwrap(), RunTarget::Native);
    }

    #[test]
    fn classify_is_pulled_receives_parsed_ref() {        // is_pulled 回调收到的必须是完整解析后的引用（含 library/ 补全与 tag）
        let seen = std::cell::RefCell::new(None);
        let spy = |r: &crate::oci::ImageRef| {
            *seen.borrow_mut() = Some(r.clone());
            false
        };
        let _ = classify_target(Some("ubuntu:24.04"), false, spy).unwrap();
        let r = seen.borrow().clone().expect("is_pulled 应被调用");
        assert_eq!(r.registry, crate::oci::DEFAULT_REGISTRY);
        assert_eq!(r.repo, "library/ubuntu");
        assert_eq!(r.reference, "24.04");
    }

    // ---- 共享 prepare 辅助 ----

    #[test]
    fn require_cmd_rejects_empty_and_accepts_nonempty() {
        assert!(require_cmd(&[]).is_err());
        assert!(require_cmd(&["x".to_string()]).is_ok());
        // 错误提示需引导用户给出 Entrypoint/Cmd 或 `--` 后命令
        let e = require_cmd(&[]).unwrap_err();
        assert!(format!("{}", e).contains("Entrypoint/Cmd"), "{}", e);
    }

    #[test]
    fn build_sanitized_env_drops_reserved_and_applies_forced() {
        let spec_env = vec![
            ("LANG".to_string(), "C".to_string()),
            ("WBOX_VA_BITS".to_string(), "43".to_string()),
            ("BLINK_PREFIX".to_string(), "/".to_string()),
        ];
        let forced = vec![("BLINK_PREFIX".to_string(), "/rootfs".to_string())];
        let env = build_sanitized_env(&spec_env, &forced, false, false);
        // 保留键丢弃后由 forced 提供唯一 BLINK_PREFIX
        let prefix: Vec<&str> = env
            .iter()
            .filter(|(k, _)| k == "BLINK_PREFIX")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(prefix, vec!["/rootfs"]);
        assert!(!env.iter().any(|(k, _)| k == "WBOX_VA_BITS"));
        assert!(env.iter().any(|(k, v)| k == "LANG" && v == "C"));
        // SystemRoot 兜底存在（白名单路径）
        assert!(env.iter().any(|(k, _)| k.eq_ignore_ascii_case("SystemRoot")));
    }

    #[test]
    fn build_sanitized_env_pass_all_still_filters_reserved() {
        let mut g = crate::testenv::EnvGuard::new();
        g.set("WBOX_TEST_SECRET", "hunter2");
        let env = build_sanitized_env(&[], &[], true, false);
        assert!(!env.iter().any(|(k, _)| k == "WBOX_TEST_SECRET"));
    }
}
