//! NativeBackend：Windows 原生进程后端。
//!
//! 把 v1 的隔离编排（AppContainer profile + capability + Job Object +
//! attribute-list CreateProcessW）收编为 [`Backend`] 实现。
//! 同时暴露 [`spawn_native`] 供 BlinkBackend 复用——wbox-linux.exe
//! 也是 Windows 原生程序，经同一条隔离链路启动即成"双层隔离"。

use super::{Backend, Prepared, RunSpec};
use crate::error::{ErrKind, Result, WboxError};
use crate::{job, sandbox, token};

/// Windows 原生进程后端（无状态）。
pub struct NativeBackend;

impl Backend for NativeBackend {
    fn prepare(&self, spec: &RunSpec) -> Result<Prepared> {
        if spec.cmd.is_empty() {
            return Err(WboxError::args("缺少要执行的命令（-- <CMD> [ARGS...]）"));
        }
        if !spec.workdir.is_dir() {
            return Err(WboxError::args(format!(
                "工作目录 '{}' 不存在或不是目录",
                spec.workdir.display()
            )));
        }
        Ok(Prepared {
            cmd: spec.cmd.clone(),
            workdir: spec.workdir.clone(),
            env: spec.env.clone(),
        })
    }

    fn spawn(&self, spec: &RunSpec, prepared: &Prepared) -> Result<u32> {
        spawn_native(spec, prepared, "原生进程")
    }
}

/// 在 AppContainer + Job Object 隔离单元内启动 `prepared.cmd` 并等待退出。
///
/// - `spec` 提供容器名 / 限额 / capability / verbose 开关；
/// - `prepared` 提供最终命令行、工作目录与注入环境变量；
/// - `target_desc`：verbose 输出中的目标描述（原生进程 / wbox-linux 模拟器）。
pub fn spawn_native(spec: &RunSpec, prepared: &Prepared, target_desc: &str) -> Result<u32> {
    // ---- 0. 环境变量注入（CreateProcess 传 null 环境块 = 继承本进程环境，
    //    故先写入本进程环境再创建子进程；wbox 为一次性进程，无需恢复）----
    for (k, v) in &prepared.env {
        // 空 key 或含 '=' 的 key 为非法输入，防御性跳过
        if k.is_empty() || k.contains('=') {
            continue;
        }
        std::env::set_var(k, v);
    }

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
    let code = sandbox::run_container(&profile, &caps, &cmdline, &workdir, &job)?;

    if spec.verbose {
        println!("wbox: 子进程退出，退出码 = {}", code);
    }
    Ok(code)
}

// 供 main.rs 在 verbose 等处复用的错误构造辅助（保持 ErrKind 使用集中）。
#[allow(dead_code)]
pub(crate) fn spawn_err(msg: impl Into<String>) -> WboxError {
    WboxError::new(ErrKind::Spawn, anyhow::anyhow!(msg.into()))
}
