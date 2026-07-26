//! NativeBackend：Windows 原生进程后端。
//!
//! 把 v1 的隔离编排（AppContainer profile + capability + Job Object +
//! attribute-list CreateProcessW）收编为 [`Backend`] 实现。
//! 同时暴露 [`spawn_native`] 供 BlinkBackend 复用——wbox-linux.exe
//! 也是 Windows 原生程序，经同一条隔离链路启动即成"双层隔离"。

// prepare 的校验与环境构造为纯逻辑，跨平台可编译/可测；
// 仅 spawn 链路（AppContainer/Job/CreateProcessW）为 Windows 专属。
use super::{Backend, Prepared, RunSpec};
use crate::error::{Result, WboxError};
#[cfg(windows)]
use crate::{job, sandbox, token};

/// Windows 原生进程后端（无状态）。
pub struct NativeBackend;

impl Backend for NativeBackend {
    fn prepare(&self, spec: &RunSpec) -> Result<Prepared> {
        super::require_cmd(&spec.cmd)?;
        if !spec.workdir.is_dir() {
            return Err(WboxError::args(format!(
                "工作目录 '{}' 不存在或不是目录",
                spec.workdir.display()
            )));
        }
        // H2/H6：默认不继承完整宿主环境——构造显式白名单环境；
        // spec.env 中的保留键（BLINK_*/WBOX_*）过滤后并入（统一策略见
        // backend::build_sanitized_env）。
        let env = super::build_sanitized_env(&spec.env, &[], spec.env_pass_all, spec.verbose);
        Ok(Prepared {
            cmd: spec.cmd.clone(),
            workdir: spec.workdir.clone(),
            env,
        })
    }

    #[cfg(windows)]
    fn spawn(&self, spec: &RunSpec, prepared: &Prepared) -> Result<u32> {
        spawn_native(spec, prepared, "原生进程")
    }

    /// 非 Windows 平台：隔离原语为 Win32 API，明确报错（与 run_native 一致）。
    #[cfg(not(windows))]
    fn spawn(&self, _spec: &RunSpec, _prepared: &Prepared) -> Result<u32> {
        Err(WboxError::spawn(
            "原生后端仅在 Windows 上可用（AppContainer/Job Object 为 Win32 原语）",
        ))
    }
}

/// 在 AppContainer + Job Object 隔离单元内启动 `prepared.cmd` 并等待退出。
///
/// - `spec` 提供容器名 / 限额 / capability / verbose 开关；
/// - `prepared` 提供最终命令行、工作目录与注入环境变量；
/// - `target_desc`：verbose 输出中的目标描述（原生进程 / wbox-linux 模拟器）。
#[cfg(windows)]
pub fn spawn_native(spec: &RunSpec, prepared: &Prepared, target_desc: &str) -> Result<u32> {
    // ---- 0. 子进程环境：prepared.env 为 prepare 阶段构造的**显式白名单**
    //    环境（含强制覆盖项），经 lpEnvironment 直接传给 CreateProcessW，
    //    不再通过本进程 set_var 继承（默认不透传宿主环境/机密，H6）----

    // 工作目录：只做存在性校验，不 canonicalize（std 会产生 `\\?\` 扩展
    // 前缀，CreateProcessW 的 lpCurrentDirectory 不接受）。
    let workdir = prepared.workdir.to_string_lossy().into_owned();
    let workdir = workdir.strip_prefix(r"\\?\").unwrap_or(&workdir).to_string();

    // ---- 1. capability 集合（v1：仅 INTERNET_CLIENT）----
    let mut caps: Vec<token::CapabilitySid> = Vec::new();
    if spec.allow_network {
        caps.push(token::CapabilitySid::internet_client()?);
    }

    // ---- 2. AppContainer profile ----
    let mut profile = token::AppContainerProfile::create(&spec.name, &caps)?;
    if spec.keep_profile {
        profile.keep();
    }

    // ---- 3. Job Object ----
    let limits = job::JobLimits {
        memory_mb: spec.limits.memory_mb,
        cpu_pct: spec.limits.cpu_pct,
        max_procs: spec.limits.max_procs,
    };
    let job = job::Job::create(limits)?;

    // ---- 4. verbose 摘要 ----
    if spec.verbose {
        println!("wbox 隔离配置:");
        println!("  profile 名   : {}", profile.name());
        println!("  AppContainer SID: {}", profile.sid_string()?);
        println!("  完整性级别   : Low（AppContainer 派生令牌内核强制）");
        println!(
            "  capabilities : {}",
            if caps.is_empty() {
                "（无，无网络能力）".to_string()
            } else {
                caps.iter().map(|c| c.desc()).collect::<Vec<_>>().join(", ")
            }
        );
        println!(
            "  Job 限额     : KILL_ON_JOB_CLOSE=on, memory={}MB, cpu={}%, max-procs={}",
            limits.memory_mb, limits.cpu_pct, limits.max_procs
        );
        println!("  工作目录     : {}", workdir);
        println!("  执行目标     : {}", target_desc);
        if !prepared.env.is_empty() {
            println!(
                "  注入环境变量 : {}",
                prepared
                    .env
                    .iter()
                    .map(|(k, _)| k.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        println!("  keep-profile : {}", spec.keep_profile);
    }

    // ---- 5. 启动并等待 ----
    let cmdline = sandbox::build_cmdline(&prepared.cmd)?;
    let code = sandbox::run_container(&profile, &caps, &cmdline, &workdir, &job, &prepared.env)?;

    if spec.verbose {
        println!("wbox: 子进程退出，退出码 = {}", code);
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(workdir: std::path::PathBuf, cmd: &[&str]) -> RunSpec {
        RunSpec {
            name: "t".to_string(),
            limits: Default::default(),
            allow_network: false,
            keep_profile: false,
            workdir,
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            env: vec![
                ("LANG".to_string(), "C".to_string()),
                ("WBOX_VA_BITS".to_string(), "43".to_string()),
            ],
            verbose: false,
            env_pass_all: false,
        }
    }

    #[test]
    fn prepare_rejects_empty_cmd_and_missing_workdir() {
        let dir = std::env::temp_dir();
        assert!(NativeBackend.prepare(&spec(dir.clone(), &[])).is_err());
        let missing = dir.join("wbox-definitely-missing-workdir");
        let e = NativeBackend.prepare(&spec(missing, &["cmd.exe"])).unwrap_err();
        assert!(format!("{}", e).contains("工作目录"), "{}", e);
    }

    #[test]
    fn prepare_builds_sanitized_env_and_echoes_cmd() {
        let p = NativeBackend
            .prepare(&spec(std::env::temp_dir(), &["cmd.exe", "/c", "echo"]))
            .unwrap();
        assert_eq!(p.cmd, vec!["cmd.exe", "/c", "echo"]);
        // 保留键被过滤，普通键保留；SystemRoot 兜底存在
        assert!(!p.env.iter().any(|(k, _)| k == "WBOX_VA_BITS"));
        assert!(p.env.iter().any(|(k, v)| k == "LANG" && v == "C"));
        assert!(p.env.iter().any(|(k, _)| k.eq_ignore_ascii_case("SystemRoot")));
    }

    #[cfg(not(windows))]
    #[test]
    fn spawn_on_non_windows_is_explicit_error() {
        let p = NativeBackend
            .prepare(&spec(std::env::temp_dir(), &["x"]))
            .unwrap();
        let s = spec(std::env::temp_dir(), &["x"]);
        let e = NativeBackend.spawn(&s, &p).unwrap_err();
        assert!(format!("{}", e).contains("仅在 Windows"), "{}", e);
    }
}
