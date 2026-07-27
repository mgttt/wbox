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
// Linux 原生后端（见 PRD.md F5 / docs/architecture.md §3）。
// prepare 是纯逻辑，任何平台都能编译与单测；spawn 在隔离落地前明确报错。
mod linux;
/// 台阶③：Linux 上跑 Windows 程序的执行器变体（集成 wine，非新后端）。
/// 非 Linux 宿主用 `wine_stub` 顶上同名空实现——Windows 上跑 PE 本来就是原生
/// 路径，不需要 wine。这样调用方无需写任何 `cfg`。
#[cfg(target_os = "linux")]
pub mod wine;
#[cfg(not(target_os = "linux"))]
#[path = "wine_stub.rs"]
pub mod wine;
// native 的 prepare 纯逻辑跨平台可编译（spawn 链路内部 cfg），
// 使命令校验/环境构造可在 Linux 沙箱单测。
mod native;

use blink::ensure_resolv_conf;
pub use blink::BlinkBackend;
pub use linux::{LinuxMode, LinuxNativeBackend};
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

// 这两个换算只有 Windows 侧的 job.rs 消费（非 Windows 构建下无调用方），
// 但**规则本身跨平台**，故定义与单测都留在这里——与 RunSpec/Prepared 同一处理。
#[cfg_attr(not(windows), allow(dead_code))]
impl Limits {
    /// Job Object 的 `ProcessMemoryLimit` 单位是**字节**（每进程上限）。
    /// 返回 `None` 表示不限（`memory_mb == 0`）。
    ///
    /// 换算放在这里而非 job.rs：job.rs 是 `cfg(windows)` 且换算夹在 unsafe
    /// Win32 调用中间，Linux 上既编译不到也测不了。溢出必须显式失败——
    /// 静默回绕会把"上限 5 TB"变成一个很小的值，反而收紧限制且无从察觉。
    pub(crate) fn memory_limit_bytes(&self) -> Result<Option<usize>> {
        if self.memory_mb == 0 {
            return Ok(None);
        }
        usize::try_from(self.memory_mb)
            .ok()
            .and_then(|mb| mb.checked_mul(1024 * 1024))
            .map(Some)
            .ok_or_else(|| {
                crate::error::WboxError::job(format!(
                    "内存上限溢出：{} MB 无法用本平台的 usize 表示为字节",
                    self.memory_mb
                ))
            })
    }

    /// `JOBOBJECT_CPU_RATE_CONTROL_INFORMATION.CpuRate` 的语义是
    /// **百分比 × 100**（每周期可用时间比例）。返回 `None` 表示不限。
    pub(crate) fn cpu_rate(&self) -> Option<u32> {
        if self.cpu_pct == 0 {
            None
        } else {
            Some(self.cpu_pct * 100)
        }
    }
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
    /// 卷 / 绑定挂载（PRD F9.1）
    pub volumes: Vec<VolumeMount>,
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

/// `CreateAppContainerProfile` 对 `pszAppContainerName` 的长度上限（字符）。
/// 见 Win32 文档；超限只会得到 E_INVALIDARG，与其他参数错误无法区分。
pub(crate) const MAX_CONTAINER_NAME_CHARS: usize = 64;

/// 校验容器名（= AppContainer profile 名）。
///
/// 放在跨平台层而非 `token.rs`：一来两个后端最终都经 AppContainer 启动，
/// 约束一致；二来 `token.rs` 是 `cfg(windows)`，规则若只写在那里，
/// Linux 上的 `cargo test` 永远覆盖不到。
pub(crate) fn validate_container_name(name: &str) -> Result<()> {
    // 按**字符**而非字节计数：非 ASCII 名字按字节判会误伤。
    let len = name.chars().count();
    if len == 0 || len > MAX_CONTAINER_NAME_CHARS {
        return Err(crate::error::WboxError::args(format!(
            "容器名长度非法（{} 字符）：AppContainer profile 名须为 1..={} 字符",
            len, MAX_CONTAINER_NAME_CHARS
        )));
    }
    Ok(())
}

/// 校验镜像 rootfs 目录存在。
///
/// Blink 与 LinuxNative 两个镜像后端各自写过一遍几乎一样的检查与文案；
/// 收到这里保证**报错口径一致**——同一个"没 pull 成功"的处境，不该因为
/// 宿主不同而给出两种说法。
pub(crate) fn require_rootfs_dir(rootfs: &std::path::Path) -> Result<()> {
    if !rootfs.is_dir() {
        return Err(crate::error::WboxError::registry(format!(
            "镜像 rootfs 目录 '{}' 不存在（是否已成功 pull？）",
            rootfs.display()
        )));
    }
    Ok(())
}

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
    flavor: env::GuestFlavor,
) -> Vec<(String, String)> {
    let (img_env, dropped) = env::sanitize_image_env(spec_env);
    if verbose && !dropped.is_empty() {
        println!(
            "wbox: 已丢弃环境变量中的保留键（隔离/凭证相关）：{}",
            dropped.join(", ")
        );
    }
    env::build_child_env(&img_env, forced, pass_all, flavor)
}

/// 镜像目标在**当前宿主**上应走的后端。
///
/// Windows 宿主：guest 是 Linux ELF，宿主跑不了，必须经 wbox-linux 模拟
/// （BlinkBackend），外层再套 AppContainer+Job。
/// Linux 宿主：宿主本身就能执行 Linux ELF，走原生 namespace 隔离
/// （LinuxNativeBackend），无需模拟器——见 docs/architecture.md §3。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageBackendKind {
    /// 模拟执行（wbox-linux / blink）
    Blink,
    /// 宿主原生执行（Linux namespace）
    LinuxNative,
}

/// 按宿主选择镜像后端。单独成函数是为了可测——分派规则本身能被断言，
/// 而不是散落在 cli 的 cfg 分支里。
pub const fn image_backend_kind() -> ImageBackendKind {
    if cfg!(windows) {
        ImageBackendKind::Blink
    } else {
        ImageBackendKind::LinuxNative
    }
}

/// **宿主程序**目标（`wbox run -- <本机程序>`）在当前宿主上应走的后端。
///
/// 与 [`ImageBackendKind`] 是两条独立的分派：那条决定"Linux ELF 怎么跑"，
/// 这条决定"本机程序用哪套隔离原语包起来"。
/// Windows：AppContainer + Job Object；Linux：user/pid/net namespace + cgroup。
/// 其它宿主暂无实现——明确报错，不假装成功（PRD F5 一致性要求）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostProgramBackendKind {
    /// Win32：AppContainer profile + Job Object
    AppContainer,
    /// Linux：namespace + cgroup（rootless）
    LinuxNamespace,
    /// 本宿主没有可用的隔离原语
    Unsupported,
}

/// 按宿主选择宿主程序后端。与 [`image_backend_kind`] 同理单独成函数以便断言。
pub const fn host_program_backend_kind() -> HostProgramBackendKind {
    if cfg!(windows) {
        HostProgramBackendKind::AppContainer
    } else if cfg!(target_os = "linux") {
        HostProgramBackendKind::LinuxNamespace
    } else {
        HostProgramBackendKind::Unsupported
    }
}

/// `run` 的执行目标判别结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunTarget {
    /// 本地 Windows 可执行路径（v1 行为）
    Native,
    /// OCI 镜像引用（已解析；缓存缺失时由 run 自动拉取）
    Image(crate::oci::ImageRef),
}

/// 判别"镜像引用 vs 本地可执行路径"。
///
/// Docker/Podman 语义下首个位置参数默认是镜像引用。明确的相对/绝对路径、
/// Windows 可执行文件名仍作为本地程序；无歧义的本地命令推荐写成
/// `wbox run -- PROGRAM [ARGS...]`。绝不能因镜像未缓存而静默回退宿主执行。
pub fn classify_target(positional: Option<&str>) -> Result<RunTarget> {
    let Some(s) = positional else {
        return Ok(RunTarget::Native);
    };
    if looks_like_native_program(s) {
        return Ok(RunTarget::Native);
    }
    crate::oci::ImageRef::parse(s, None).map(RunTarget::Image)
}

fn looks_like_native_program(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    s.starts_with('/')
        || s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with(".\\")
        || s.starts_with("..\\")
        || s.contains('\\')
        || (s.len() >= 2 && s.as_bytes()[1] == b':')
        || [".exe", ".com", ".bat", ".cmd", ".ps1"]
            .iter()
            .any(|ext| lower.ends_with(ext))
}

#[cfg(test)]
mod tests {
    // ---- 宿主分派（docs/architecture.md §3）----
    #[test]
    fn image_backend_follows_host() {
        let k = super::image_backend_kind();
        if cfg!(windows) {
            assert_eq!(k, super::ImageBackendKind::Blink, "Windows 宿主必须走模拟器");
        } else {
            assert_eq!(
                k,
                super::ImageBackendKind::LinuxNative,
                "Linux 宿主应走原生 namespace，而非白白多套一层模拟"
            );
        }
    }

    /// 宿主程序目标（台阶①）也必须按宿主分派——Linux 上曾经直接报
    /// "原生后端仅在 Windows 上可用"，等于台阶① 完全缺失。
    #[test]
    fn host_program_backend_follows_host() {
        let k = super::host_program_backend_kind();
        if cfg!(windows) {
            assert_eq!(k, super::HostProgramBackendKind::AppContainer);
        } else if cfg!(target_os = "linux") {
            assert_eq!(
                k,
                super::HostProgramBackendKind::LinuxNamespace,
                "Linux 宿主必须能沙箱宿主程序（harness 环境控制的基础）"
            );
        } else {
            assert_eq!(k, super::HostProgramBackendKind::Unsupported);
        }
    }

    use super::*;

    #[test]
    fn no_positional_is_native() {
        assert_eq!(classify_target(None).unwrap(), RunTarget::Native);
    }

    #[test]
    fn image_ref_is_image_regardless_of_cache_state() {
        match classify_target(Some("ubuntu:24.04")).unwrap() {
            RunTarget::Image(r) => {
                assert_eq!(r.repo, "library/ubuntu");
                assert_eq!(r.reference, "24.04");
            }
            other => panic!("期望 Image，得到 {:?}", other),
        }
    }

    #[test]
    fn local_exe_path_stays_native() {
        // 明确的本地程序名/路径不会被误判为镜像。
        for s in ["cmd.exe", "notepad.exe", r"C:\tools\app.exe", "./run.sh"] {
            assert_eq!(
                classify_target(Some(s)).unwrap(),
                RunTarget::Native,
                "{}",
                s
            );
        }
    }

    // ---- classify_target 边界：ubuntu vs ./ubuntu vs ubuntu:latest vs 绝对路径 ----

    #[test]
    fn classify_bare_name_tagged_and_path_forms() {
        match classify_target(Some("ubuntu")).unwrap() {
            RunTarget::Image(r) => assert_eq!(r.reference, "latest"),
            other => panic!("期望 Image，得到 {:?}", other),
        }
        // "ubuntu:latest" 同样 → Image
        match classify_target(Some("ubuntu:latest")).unwrap() {
            RunTarget::Image(r) => assert_eq!(r.reference, "latest"),
            other => panic!("期望 Image，得到 {:?}", other),
        }
        for s in ["./ubuntu", "/usr/bin/ubuntu", r"C:\ubuntu\app.exe"] {
            assert_eq!(
                classify_target(Some(s)).unwrap(),
                RunTarget::Native,
                "{} 必须保持 Native",
                s
            );
        }
    }

    #[test]
    fn classify_relative_path_is_always_native() {
        assert_eq!(
            classify_target(Some("./ubuntu")).unwrap(),
            RunTarget::Native
        );
    }

    #[test]
    fn classify_absolute_path_never_image() {
        for s in ["/usr/bin/foo", r"C:\x\y.exe", r"D:\tools"] {
            assert_eq!(
                classify_target(Some(s)).unwrap(),
                RunTarget::Native,
                "{}",
                s
            );
        }
    }

    #[test]
    fn classify_none_positional_is_native() {
        assert_eq!(classify_target(None).unwrap(), RunTarget::Native);
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
        let env = build_sanitized_env(&spec_env, &forced, false, false, env::GuestFlavor::Windows);
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

    // ---- Limits → Win32 数值换算 ----

    #[test]
    fn memory_limit_zero_means_unlimited() {
        let l = Limits { memory_mb: 0, ..Default::default() };
        assert_eq!(l.memory_limit_bytes().unwrap(), None);
    }

    #[test]
    fn memory_limit_converts_mb_to_bytes() {
        let l = Limits { memory_mb: 256, ..Default::default() };
        assert_eq!(l.memory_limit_bytes().unwrap(), Some(256 * 1024 * 1024));
    }

    #[test]
    fn memory_limit_overflow_is_error_not_wraparound() {
        // 静默回绕会把"极大上限"变成一个很小的值 —— 反而收紧限制且无从察觉，
        // 故必须报错。错误须归类为 Job 错误（退出码 3）。
        let l = Limits { memory_mb: u64::MAX, ..Default::default() };
        let err = l.memory_limit_bytes().unwrap_err();
        assert_eq!(err.exit_code(), 3, "{}", err);
        assert!(format!("{}", err).contains("溢出"), "{}", err);
    }

    #[test]
    fn cpu_rate_is_percent_times_100() {
        assert_eq!(Limits { cpu_pct: 0, ..Default::default() }.cpu_rate(), None);
        assert_eq!(Limits { cpu_pct: 1, ..Default::default() }.cpu_rate(), Some(100));
        assert_eq!(Limits { cpu_pct: 50, ..Default::default() }.cpu_rate(), Some(5000));
        // CLI 已把上限卡在 100（parse 时校验），此处确认边界不越出 Win32 的 10000
        assert_eq!(Limits { cpu_pct: 100, ..Default::default() }.cpu_rate(), Some(10000));
    }

    // ---- 容器名校验（AppContainer profile 名 1..=64 字符）----

    #[test]
    fn container_name_accepts_typical_and_boundary_lengths() {
        for name in ["w", "wbox-1234", &"a".repeat(MAX_CONTAINER_NAME_CHARS)] {
            assert!(
                validate_container_name(name).is_ok(),
                "应接受长度 {} 的名字",
                name.chars().count()
            );
        }
    }

    #[test]
    fn container_name_rejects_empty_and_overlong() {
        assert!(validate_container_name("").is_err(), "空名必须拒绝");
        let too_long = "a".repeat(MAX_CONTAINER_NAME_CHARS + 1);
        let err = validate_container_name(&too_long).unwrap_err();
        // 错误须归类为参数错误（退出码 1），而不是 profile 错误（退出码 2）：
        // 这是用户输入问题，不是 AppContainer 子系统问题。
        assert_eq!(err.exit_code(), 1, "{}", err);
        assert!(format!("{}", err).contains("65"), "错误应报出实际长度：{}", err);
    }

    #[test]
    fn container_name_counts_chars_not_bytes() {
        // 64 个中文字符 = 192 字节；按字节判会误拒。Win32 的限制是字符数。
        let cjk = "容".repeat(MAX_CONTAINER_NAME_CHARS);
        assert_eq!(cjk.len(), MAX_CONTAINER_NAME_CHARS * 3, "前提：每字符 3 字节");
        assert!(validate_container_name(&cjk).is_ok(), "64 个非 ASCII 字符应接受");
        let over = "容".repeat(MAX_CONTAINER_NAME_CHARS + 1);
        assert!(validate_container_name(&over).is_err(), "65 个字符应拒绝");
    }

    #[test]
    fn build_sanitized_env_pass_all_still_filters_reserved() {
        let mut g = crate::testenv::EnvGuard::new();
        g.set("WBOX_TEST_SECRET", "hunter2");
        let env = build_sanitized_env(&[], &[], true, false, env::GuestFlavor::Windows);
        assert!(!env.iter().any(|(k, _)| k == "WBOX_TEST_SECRET"));
    }
}

// ---------------------------------------------------------------------------
// 卷 / 绑定挂载（PRD F9.1）
// ---------------------------------------------------------------------------

/// 卷挂载在**当前宿主**是否可用；不可用时给出明确理由。
///
/// Linux 走 mount namespace 里的 bind mount（`linux_ns` 已实现）。Windows 侧
/// 没有等价的用户态手段——AppContainer 不提供路径重定向，那需要 minifilter
/// 驱动，而 wbox 明确不装驱动（PRD §2.3 / §2.4 天花板一）。**静默忽略 `-v`
/// 比不支持更糟**：用户会以为目录已经挂进去了。
pub fn reject_volumes_if_unsupported(volumes: &[VolumeMount]) -> Result<()> {
    if volumes.is_empty() || cfg!(target_os = "linux") {
        return Ok(());
    }
    Err(crate::error::WboxError::args(
        "-v 卷挂载目前只在 Linux 宿主可用：Windows 侧需要文件系统重定向，         而那要 minifilter 驱动（PRD §2.4 天花板一，取证见 §4.9 W3）",
    ))
}

/// 一条 `-v host:guest[:ro]` 挂载。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeMount {
    /// 宿主侧路径（必须已存在——自动创建会把拼错的路径变成一个空目录，
    /// 用户直到发现数据"不见了"才知道挂错了）
    pub host: PathBuf,
    /// 容器内挂载点（绝对路径）
    pub guest: String,
    pub read_only: bool,
}

/// 解析 `-v` 取值。
///
/// 形如 `host:guest`、`host:guest:ro`、`host:guest:rw`。从右往左先剥离
/// 模式，再识别容器路径，因此 Windows 宿主路径的盘符冒号不会被误当分隔符。
pub fn parse_volume(spec: &str) -> Result<VolumeMount> {
    use crate::error::WboxError;
    let bad = |why: &str| {
        WboxError::args(format!(
            "-v '{}' 无效：{}（用法 host:guest[:ro|:rw]）",
            spec, why
        ))
    };
    let (body, mode) = match spec.rsplit_once(':') {
        Some((body, "ro")) => (body, Some("ro")),
        Some((body, "rw")) => (body, Some("rw")),
        _ => (spec, None),
    };
    let (host, guest) = body.rsplit_once(':').ok_or_else(|| bad("缺少容器路径分隔符"))?;
    let read_only = match mode {
        None | Some("rw") => false,
        Some("ro") => true,
        Some(m) => return Err(bad(&format!("未知模式 '{}'", m))),
    };
    if host.is_empty() || guest.is_empty() {
        return Err(bad("宿主路径与容器路径都不能为空"));
    }
    if !guest.starts_with('/') {
        return Err(bad("容器路径必须是绝对路径"));
    }
    // 安全断言：`-v /somewhere:/` 会把宿主目录盖在容器根上，等于把隔离作废。
    // 这条不是防手滑，是防"一条命令就让沙箱失效"。
    if guest == "/" {
        return Err(bad("不允许挂载到容器根 '/'——那会让隔离失效"));
    }
    let host_path = PathBuf::from(host);
    if !host_path.exists() {
        return Err(bad("宿主路径不存在（wbox 不会替你创建：拼错的路径会变成一个\
                        空目录，等你发现数据不见了才知道挂错了）"));
    }
    let host_path = host_path.canonicalize().map_err(|e| {
        WboxError::args(format!("-v '{}'：解析宿主路径失败：{}", spec, e))
    })?;
    Ok(VolumeMount {
        host: host_path,
        guest: guest.to_string(),
        read_only,
    })
}

#[cfg(test)]
mod volume_tests {
    use super::*;

    #[test]
    fn parses_basic_and_modes() {
        let tmp = std::env::temp_dir();
        let t = tmp.to_str().unwrap();
        let v = parse_volume(&format!("{}:/data", t)).unwrap();
        assert_eq!(v.guest, "/data");
        assert!(!v.read_only);
        assert!(parse_volume(&format!("{}:/data:ro", t)).unwrap().read_only);
        assert!(!parse_volume(&format!("{}:/data:rw", t)).unwrap().read_only);
    }

    /// 挂到容器根上等于把隔离作废——必须拒绝。
    #[test]
    fn rejects_mount_over_container_root() {
        let t = std::env::temp_dir();
        let e = parse_volume(&format!("{}:/", t.to_str().unwrap())).unwrap_err();
        assert!(format!("{}", e).contains("隔离失效"), "{}", e);
    }

    #[test]
    fn rejects_malformed_specs() {
        let t = std::env::temp_dir();
        let t = t.to_str().unwrap();
        assert!(parse_volume("nocolon").is_err(), "缺冒号");
        assert!(parse_volume(&format!("{}:relative", t)).is_err(), "容器路径须绝对");
        assert!(parse_volume(&format!("{}:/d:bogus", t)).is_err(), "未知模式");
        assert!(parse_volume(&format!("{}:/d:ro:extra", t)).is_err(), "段数过多");
        assert!(parse_volume(":/d").is_err(), "空宿主路径");
    }

    /// 宿主路径不存在要**报错而不是自动创建**，且错误要说明为什么。
    #[test]
    fn missing_host_path_explains_why_not_created() {
        let e = parse_volume("/definitely/not/here/xyz:/d").unwrap_err();
        let m = format!("{}", e);
        assert!(m.contains("不存在"), "{}", m);
        assert!(m.contains("不会替你创建"), "要解释为什么不自动创建：{}", m);
    }
}

#[cfg(test)]
mod volume_support_tests {
    use super::*;

    /// 不带 `-v` 时任何宿主都不该报错。
    #[test]
    fn no_volumes_is_always_ok() {
        assert!(reject_volumes_if_unsupported(&[]).is_ok());
    }

    /// 带 `-v` 时：Linux 放行，其余宿主必须**明确报错**而不是静默忽略
    /// ——静默忽略会让用户以为目录已经挂进去了。这条在两个平台都跑，
    /// Windows 那半由 CI 的 windows runner 真实执行。
    #[test]
    fn volumes_rejected_off_linux_and_reason_is_actionable() {
        let v = vec![VolumeMount {
            host: PathBuf::from("/tmp"),
            guest: "/data".to_string(),
            read_only: false,
        }];
        let r = reject_volumes_if_unsupported(&v);
        if cfg!(target_os = "linux") {
            assert!(r.is_ok(), "Linux 应支持卷挂载");
        } else {
            let e = r.unwrap_err();
            let m = format!("{}", e);
            assert!(m.contains("只在 Linux"), "要说清哪个宿主可用：{}", m);
            assert!(m.contains("驱动"), "要说清为什么做不到：{}", m);
        }
    }
}
