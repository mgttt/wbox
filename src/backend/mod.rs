//! 后端抽象：把 `wbox run` 的执行目标（本地 Windows 程序 / OCI 镜像）
//! 分派到不同的运行时后端。
//!
//! - [`NativeBackend`]：包装现有 AppContainer + Job Object 隔离逻辑，
//!   直接运行 Windows 原生程序（v1 能力）。
//! - [`EmuBackend`]：Linux 用户态模拟后端骨架。定位 `wbox-linux.exe`
//!   （纯 Rust 模拟器，exe 同目录或 `WBOX_LINUX` 环境变量），
//!   构造 rootfs / `BLINK_PREFIX` 参数后，**仍经 NativeBackend 的
//!   AppContainer 拉起 wbox-linux.exe**（双层隔离：外层 AppContainer
//!   关住模拟器，模拟器再关住 guest Linux 进程）。
//!   找不到 exe 时给出明确错误，不静默降级。
//!
//! 该模块跨平台可编译（Windows 专属部分在 native.rs / emu.rs 内 cfg），
//! 使命令行合并、exe 定位等纯逻辑能在 Linux 沙箱单测。

mod emu;
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

#[cfg(windows)]
pub(crate) use emu::copy_rootfs_tree;
#[cfg(windows)]
pub(crate) use emu::create_private_rootfs;
use emu::ensure_resolv_conf;
pub use emu::EmuBackend;
/// rootless overlay 是否可用（build 用它决定能否硬链接铺基础层，PRD L5b）。
#[cfg(target_os = "linux")]
pub(crate) use linux::ns::rootless_overlay_available;
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
///
/// 实现了 [`Default`]：新增字段时调用方只需在**关心它的地方**写出来，
/// 其余用 `..RunSpec::default()` 补齐。此前每加一个字段都要改七处
/// 结构体字面量，且每处都在重复同一串"不关心"的默认值。
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Default)]
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
    /// 镜像模式下是**镜像 rootfs 的宿主路径**；宿主程序模式下才是工作目录。
    /// 容器**内部**的 cwd 见 [`RunSpec::guest_workdir`]——两者是不同的东西，
    /// 曾经只有这一个字段，于是镜像模式下 `-w` 与镜像的 `WorkingDir` 都无处安放，
    /// 被静默丢掉了（F9.37 修的就是这个）。
    pub workdir: PathBuf,
    /// 宿主程序模式下把 `TMPDIR`/`TEMP`/`TMP` 指向容器私有目录（§4.9 W6）。
    ///
    /// 镜像模式不需要它：换根之后 `/tmp` 本就在容器自己的可写层里。
    pub private_tmp: bool,
    /// 容器**内**的工作目录（绝对路径）。`None` = 不指定，落在 `/`。
    ///
    /// 来源按优先级：`-w/--workdir` > 镜像 config 的 `WorkingDir`。
    ///
    /// 目前只有 Linux 镜像后端读它（换根后 `chdir`）。Q2 的 Windows 镜像路径
    /// 等纯 Rust guest VFS 落地后同样该读——那时它已经在 spec 里，不必再改结构。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub guest_workdir: Option<String>,
    /// 最终命令行（镜像模式下已按 docker 规则合并 Entrypoint/Cmd）
    pub cmd: Vec<String>,
    /// 注入子进程的环境变量（镜像 config Env；原生模式为空）
    pub env: Vec<(String, String)>,
    /// 卷 / 绑定挂载（PRD F9.1）
    pub volumes: Vec<VolumeMount>,
    /// 端口转发（PRD F9.2，仅 Linux 宿主）
    pub ports: Vec<crate::portfwd::PortMap>,
    /// 重启策略（PRD F9.6）。由 supervisor 在 guest 退出后据此决定是否重跑。
    pub restart: crate::restart::RestartPolicy,
    /// `--user UID[:GID]`（PRD F9.7，仅 Linux 宿主）。`None` = 沿用默认
    /// （rootless 下映射为容器内 root）。非 Linux 宿主在 CLI 层就已拒绝，
    /// 故那些构建里没有读者。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub user: Option<UserSpec>,
    /// capability 裁剪（PRD F9.8，仅 Linux 宿主）。默认保留全部。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub caps: crate::caps::CapPolicy,
    /// seccomp 拒绝名单（PRD F9.9，仅 Linux 宿主）。默认不装过滤器。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub seccomp: crate::seccomp::SeccompPolicy,
    /// 健康检查（PRD F9.10，仅 Linux 宿主）。与 ports/restart 同属
    /// **supervisor 侧**关注点，故与它们并列放在这里。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub health: Option<crate::health::HealthSpec>,
    /// `--network container:<NAME>`（PRD F9.11，仅 Linux 宿主）：不建新 netns，
    /// 而是加入该容器的 user+net namespace。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub network_container: Option<String>,
    /// `--ipc container:<NAME>`（PRD F9.15，仅 Linux 宿主）
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub ipc_container: Option<String>,
    /// `--uts container:<NAME>`（PRD F9.15，仅 Linux 宿主）
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub uts_container: Option<String>,
    /// `--hostname`：容器内主机名。`None` = 用容器名（与 docker 一致）。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub hostname: Option<String>,
    /// **直写 rootfs**（不套 overlay 可写层）。`build` 的 `RUN` 步骤必须置真：
    /// 它的写入就是产物，引到 upper 层等于把 RUN 的效果全丢掉。
    /// 普通 `run` 保持默认 false——容器写入不该污染共享镜像缓存（F9.12）。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub direct_rootfs_writes: bool,
    /// 显式指定 overlay 可写层目录（PRD L5b）。`None` = 用容器状态目录下的
    /// `layer/`（F9.12 的默认）。build 需要自己掌控 upper 的位置，
    /// 因为它要在步骤结束后把 upper 合并回 staging。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub overlay_layer_dir: Option<PathBuf>,
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

// ---- 后端共享的 prepare 辅助（native / emu 公共逻辑下沉）----

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
/// 模拟器与 LinuxNative 两个镜像后端各自写过一遍几乎一样的检查与文案；
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
/// （EmuBackend），外层再套 AppContainer+Job。
/// Linux 宿主：宿主本身就能执行 Linux ELF，走原生 namespace 隔离
/// （LinuxNativeBackend），无需模拟器——见 docs/architecture.md §3。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageBackendKind {
    /// 用户态模拟执行（wbox-linux，纯 Rust 模拟器）
    Emu,
    /// 宿主原生执行（Linux namespace）
    LinuxNative,
}

/// 按宿主选择镜像后端。单独成函数是为了可测——分派规则本身能被断言，
/// 而不是散落在 cli 的 cfg 分支里。
pub const fn image_backend_kind() -> ImageBackendKind {
    if cfg!(windows) {
        ImageBackendKind::Emu
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

/// Windows 盘符路径：`C:`、`C:\dir`、`C:/dir`。
///
/// 判据里**必须带分隔符**（或整串就是 `C:`）。原先只看"第二个字节是冒号"，
/// 于是任何单字母仓库名的镜像引用都被误判成本机程序——`wbox run c:v1` 会去
/// 找一个叫 `c:v1` 的可执行文件然后报"文件不存在"，而镜像明明就在缓存里。
/// 真实踩到过：本地构建 `-t c:v1` 之后 run 不起来。
fn looks_like_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return false;
    }
    // `C:` 本身是盘符；再长就必须紧跟路径分隔符
    b.len() == 2 || b[2] == b'\\' || b[2] == b'/'
}

fn looks_like_native_program(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    s.starts_with('/')
        || s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with(".\\")
        || s.starts_with("..\\")
        || s.contains('\\')
        || looks_like_drive_path(s)
        || [".exe", ".com", ".bat", ".cmd", ".ps1"]
            .iter()
            .any(|ext| lower.ends_with(ext))
}

#[cfg(test)]
mod drive_path_tests {
    use super::looks_like_drive_path;

    /// 真正的盘符路径要认出来。
    #[test]
    fn recognizes_real_drive_paths() {
        for p in ["C:", "c:", r"C:\Windows", "D:/tmp", r"z:\a\b"] {
            assert!(looks_like_drive_path(p), "'{}' 应判为盘符路径", p);
        }
    }

    /// **镜像引用不能被误判成盘符路径。** 原先只看"第二字节是冒号"，
    /// 于是 `c:v1` 这类单字母仓库名的镜像会被当成本机程序，run 时报
    /// "文件不存在"——而镜像就在缓存里。本地构建 `-t c:v1` 时真踩到过。
    #[test]
    fn image_refs_are_not_drive_paths() {
        for r in ["c:v1", "a:latest", "ubuntu:24.04", "x:1", "1:2"] {
            assert!(!looks_like_drive_path(r), "'{}' 不该判为盘符路径", r);
        }
    }
}

#[cfg(test)]
mod tests {
    // ---- 宿主分派（docs/architecture.md §3）----
    #[test]
    fn image_backend_follows_host() {
        let k = super::image_backend_kind();
        if cfg!(windows) {
            assert_eq!(k, super::ImageBackendKind::Emu, "Windows 宿主必须走模拟器");
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
        assert!(env
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("SystemRoot")));
    }

    // ---- Limits → Win32 数值换算 ----

    #[test]
    fn memory_limit_zero_means_unlimited() {
        let l = Limits {
            memory_mb: 0,
            ..Default::default()
        };
        assert_eq!(l.memory_limit_bytes().unwrap(), None);
    }

    #[test]
    fn memory_limit_converts_mb_to_bytes() {
        let l = Limits {
            memory_mb: 256,
            ..Default::default()
        };
        assert_eq!(l.memory_limit_bytes().unwrap(), Some(256 * 1024 * 1024));
    }

    #[test]
    fn memory_limit_overflow_is_error_not_wraparound() {
        // 静默回绕会把"极大上限"变成一个很小的值 —— 反而收紧限制且无从察觉，
        // 故必须报错。错误须归类为 Job 错误（退出码 3）。
        let l = Limits {
            memory_mb: u64::MAX,
            ..Default::default()
        };
        let err = l.memory_limit_bytes().unwrap_err();
        assert_eq!(err.exit_code(), 3, "{}", err);
        assert!(format!("{}", err).contains("溢出"), "{}", err);
    }

    #[test]
    fn cpu_rate_is_percent_times_100() {
        assert_eq!(
            Limits {
                cpu_pct: 0,
                ..Default::default()
            }
            .cpu_rate(),
            None
        );
        assert_eq!(
            Limits {
                cpu_pct: 1,
                ..Default::default()
            }
            .cpu_rate(),
            Some(100)
        );
        assert_eq!(
            Limits {
                cpu_pct: 50,
                ..Default::default()
            }
            .cpu_rate(),
            Some(5000)
        );
        // CLI 已把上限卡在 100（parse 时校验），此处确认边界不越出 Win32 的 10000
        assert_eq!(
            Limits {
                cpu_pct: 100,
                ..Default::default()
            }
            .cpu_rate(),
            Some(10000)
        );
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
        assert!(
            format!("{}", err).contains("65"),
            "错误应报出实际长度：{}",
            err
        );
    }

    #[test]
    fn container_name_counts_chars_not_bytes() {
        // 64 个中文字符 = 192 字节；按字节判会误拒。Win32 的限制是字符数。
        let cjk = "容".repeat(MAX_CONTAINER_NAME_CHARS);
        assert_eq!(
            cjk.len(),
            MAX_CONTAINER_NAME_CHARS * 3,
            "前提：每字符 3 字节"
        );
        assert!(
            validate_container_name(&cjk).is_ok(),
            "64 个非 ASCII 字符应接受"
        );
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
    crate::error::WboxError::require_linux(
        !volumes.is_empty(),
        "-v 卷挂载",
        "Windows 侧需要文件系统重定向，而那要 minifilter 驱动（PRD §2.4 天花板一，取证见 §4.9 W3）",
    )
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
    let (host, guest) = body
        .rsplit_once(':')
        .ok_or_else(|| bad("缺少容器路径分隔符"))?;
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
    // 安全断言二：容器路径里不许有 `..`。
    //
    // 落地时目标是 `rootfs.join(guest.trim_start_matches('/'))`，
    // 所以 `-v /tmp/x:/../../etc` 会 join 成 `<rootfs>/../../etc`——
    // **指到 rootfs 外面的宿主目录**，而且随后的 `create_dir_all` 会真的
    // 在宿主上把它建出来，bind mount 也照挂。只查前导 '/' 挡不住这个。
    //
    // 这里选择**直接拒绝**而不是规范化后接受：`-v` 的目标是用户显式写的，
    // 带 `..` 一定是笔误或攻击，静默改写成别的路径只会让人更迷惑。
    if std::path::Path::new(guest)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(bad("容器路径不允许包含 '..'——它会让挂载点落到容器根之外"));
    }
    // 不含路径分隔符 = **命名卷**（F9.35），与 docker 同一条规则。
    // 这条规则要简单且可预测，它决定了"数据写到哪"；任何"先当路径试试、
    // 不存在就当名字"的聪明做法，都会把一次手滑变成一个新建的空卷。
    if crate::volume::looks_like_name(host) {
        let (dir, fresh) = crate::volume::create(host)?;
        if fresh {
            // 隐式建卷要出声。docker 也隐式建，但沉默地建会让 `-v mydta:/data`
            // 这种手滑变成"数据不见了"——说一句，用户当场就能发现名字打错了。
            println!("wbox: 已创建命名卷 '{}'（{}）", host, dir.display());
        }
        return Ok(VolumeMount {
            host: dir,
            guest: guest.to_string(),
            read_only,
        });
    }
    let host_path = PathBuf::from(host);
    if !host_path.exists() {
        return Err(bad(
            "宿主路径不存在（wbox 不会替你创建：拼错的路径会变成一个\
                        空目录，等你发现数据不见了才知道挂错了）；\
                        若本意是命名卷，写成不含 '/' 的名字即可（如 -v mydata:/data）",
        ));
    }
    let host_path = host_path
        .canonicalize()
        .map_err(|e| WboxError::args(format!("-v '{}'：解析宿主路径失败：{}", spec, e)))?;
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

    /// **容器路径里的 `..` 会让挂载点落到 rootfs 之外。**
    ///
    /// 落地时是 `rootfs.join(guest.trim_start_matches('/'))`，所以
    /// `/../../etc` 会 join 成 `<rootfs>/../../etc`——指到宿主目录，
    /// 而且随后的 `create_dir_all` 会真的在宿主上建出来。只查前导 '/'
    /// 挡不住它。
    #[test]
    fn rejects_parent_dir_traversal_in_container_path() {
        let t = std::env::temp_dir();
        let t = t.to_str().unwrap();
        for guest in ["/../escape", "/a/../../escape", "/data/..", "/../"] {
            let e = match parse_volume(&format!("{t}:{guest}")) {
                Err(e) => e,
                Ok(v) => panic!("'{guest}' 应被拒绝，却解析成了 {:?}", v.guest),
            };
            assert!(
                format!("{e}").contains(".."),
                "{guest} 的报错没点明 '..'：{e}"
            );
        }
        // 正常路径仍要通过——只测"挡得住"会让恒失败的实现也变绿。
        assert_eq!(parse_volume(&format!("{t}:/data")).unwrap().guest, "/data");
        assert_eq!(
            parse_volume(&format!("{t}:/a/b/c")).unwrap().guest,
            "/a/b/c"
        );
    }

    #[test]
    fn rejects_malformed_specs() {
        let t = std::env::temp_dir();
        let t = t.to_str().unwrap();
        assert!(parse_volume("nocolon").is_err(), "缺冒号");
        assert!(
            parse_volume(&format!("{}:relative", t)).is_err(),
            "容器路径须绝对"
        );
        assert!(
            parse_volume(&format!("{}:/d:bogus", t)).is_err(),
            "未知模式"
        );
        assert!(
            parse_volume(&format!("{}:/d:ro:extra", t)).is_err(),
            "段数过多"
        );
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

// ---------------------------------------------------------------------------
// `--user`（PRD F9.7）
// ---------------------------------------------------------------------------

/// `--user UID[:GID]` 指定 guest 内的身份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserSpec {
    pub uid: u32,
    pub gid: u32,
}

/// 解析 `--user`。**只接受数字**。
///
/// 用户名要查 rootfs 里的 `/etc/passwd` 才能解析，而那时机很尴尬：镜像模式下
/// 换根尚未发生，宿主模式下压根没有"容器的 passwd"。与其做一个只在部分场景
/// 正确的名字解析，不如明说只收数字——docker 在无法解析名字时也是直接报错。
pub fn parse_user(spec: &str) -> Result<UserSpec> {
    use crate::error::WboxError;
    let bad =
        |why: &str| WboxError::args(format!("--user '{}' 无效：{}（用法 UID[:GID]）", spec, why));
    let num = |s: &str| -> Result<u32> {
        if s.is_empty() {
            return Err(bad("缺少数值"));
        }
        s.parse::<u32>().map_err(|_| {
            bad("只接受数字 UID/GID；用户名需要查容器内的 /etc/passwd，\
                 而换根尚未发生时无从解析")
        })
    };
    let (u, g) = match spec.split_once(':') {
        Some((u, g)) => (num(u)?, num(g)?),
        None => {
            let u = num(spec)?;
            (u, u)
        }
    };
    Ok(UserSpec { uid: u, gid: g })
}

/// `--user` 在**当前宿主**是否可用。
pub fn reject_user_if_unsupported(user: Option<UserSpec>) -> Result<()> {
    crate::error::WboxError::require_linux(
        user.is_some(),
        "--user",
        "它靠 user namespace 的 uid/gid 映射实现，Windows 侧的 AppContainer 没有对应语义",
    )
}

// ---------------------------------------------------------------------------
// `--network container:`（PRD F9.11）
// ---------------------------------------------------------------------------

/// 校验 `--network container:<peer>` 的组合合法性。
///
/// 三个冲突都属于"两个来源在争同一件事"，静默让一方胜出会把另一方变成谎言：
///
/// - `--allow-network`/`--network host`：网络只能来自一个地方——peer 或宿主。
/// - `-p`：端口转发线程要 setns 进**本容器自己**的 netns，加入模式下那是
///   peer 的网络；要发布端口应加在拥有网络的那个容器上。
/// - `--user`：加入模式复用 peer 的 user namespace，uid 映射是 peer 建立的，
///   本容器写不了第二份。
pub fn reject_network_container_conflicts(
    peer: Option<&str>,
    allow_network: bool,
    ports: &[crate::portfwd::PortMap],
    user: Option<UserSpec>,
) -> Result<()> {
    use crate::error::WboxError;
    let Some(peer) = peer else { return Ok(()) };
    WboxError::require_linux(
        true,
        "--network container:",
        "它靠加入目标容器的 network namespace 实现，Windows 侧没有对应原语",
    )?;
    if allow_network {
        return Err(WboxError::args(format!(
            "--network container:{} 与 --allow-network/--network host 冲突：网络只能来自一个地方（目标容器或宿主）",
            peer
        )));
    }
    if !ports.is_empty() {
        return Err(WboxError::args(format!(
            "--network container:{} 与 -p 冲突：本容器没有自己的网络可发布，请把 -p 加在拥有网络的容器 '{}' 上",
            peer, peer
        )));
    }
    if user.is_some() {
        return Err(WboxError::args(format!(
            "--network container:{} 与 --user 冲突：加入模式复用目标容器的user namespace，uid 映射由它建立，本容器无法另写一份",
            peer
        )));
    }
    Ok(())
}

#[cfg(test)]
mod network_container_tests {
    use super::*;

    fn pm() -> crate::portfwd::PortMap {
        crate::portfwd::parse_port("8080:80").unwrap()
    }

    #[test]
    fn no_peer_is_always_fine() {
        assert!(reject_network_container_conflicts(
            None,
            true,
            &[pm()],
            Some(UserSpec { uid: 1, gid: 1 })
        )
        .is_ok());
    }

    /// 三种冲突各自要报错并说清**为什么**——静默让一方胜出会把另一方变成谎言。
    #[cfg(target_os = "linux")]
    #[test]
    fn each_conflict_is_rejected_with_reason() {
        let m = format!(
            "{}",
            reject_network_container_conflicts(Some("a"), true, &[], None).unwrap_err()
        );
        assert!(m.contains("网络只能来自一个地方"), "{}", m);

        let m = format!(
            "{}",
            reject_network_container_conflicts(Some("a"), false, &[pm()], None).unwrap_err()
        );
        assert!(m.contains("拥有网络的容器"), "{}", m);

        let m = format!(
            "{}",
            reject_network_container_conflicts(
                Some("a"),
                false,
                &[],
                Some(UserSpec { uid: 5, gid: 5 })
            )
            .unwrap_err()
        );
        assert!(m.contains("user namespace"), "{}", m);

        assert!(reject_network_container_conflicts(Some("a"), false, &[], None).is_ok());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn rejected_off_linux_with_reason() {
        let m = format!(
            "{}",
            reject_network_container_conflicts(Some("a"), false, &[], None).unwrap_err()
        );
        assert!(m.contains("只在 Linux"), "{}", m);
    }
}

#[cfg(test)]
mod user_tests {
    use super::*;

    #[test]
    fn parses_uid_and_optional_gid() {
        assert_eq!(
            parse_user("1000").unwrap(),
            UserSpec {
                uid: 1000,
                gid: 1000
            }
        );
        assert_eq!(
            parse_user("1000:2000").unwrap(),
            UserSpec {
                uid: 1000,
                gid: 2000
            }
        );
        assert_eq!(parse_user("0").unwrap(), UserSpec { uid: 0, gid: 0 });
    }

    /// 用户名要报错并说清为什么——静默当成 0 会让用户以为降权成功了。
    #[test]
    fn rejects_names_with_reason() {
        let e = parse_user("nobody").unwrap_err();
        let m = format!("{}", e);
        assert!(m.contains("只接受数字"), "{}", m);
        assert!(m.contains("/etc/passwd"), "要解释为什么不支持名字：{}", m);
    }

    #[test]
    fn rejects_malformed() {
        for bad in ["", ":", "1:", ":2", "-1", "a:b", "1:2:3"] {
            assert!(parse_user(bad).is_err(), "'{}' 应被拒绝", bad);
        }
    }
}
