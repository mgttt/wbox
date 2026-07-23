//! BlinkBackend：Linux 用户态模拟后端（骨架）。
//!
//! wbox-linux.exe 是 blink（x86-64 Linux 用户态模拟器）的 wbox Win32 移植版
//! （vendor/blink，移植进行中）。本后端职责：
//! 1. 定位 wbox-linux.exe：`WBOX_LINUX` 环境变量优先，其次 wbox.exe 同目录；
//! 2. 构造执行计划：rootfs 目录作为工作目录与 `BLINK_PREFIX`（blink VFS 根），
//!    guest 命令（已按 docker 规则合并 Entrypoint/Cmd）作为 wbox-linux 的参数；
//! 3. spawn 委托 NativeBackend —— wbox-linux.exe 本身是 Windows 程序，
//!    跑在 AppContainer + Job 内形成双层隔离。
//!
//! 移植未完成前的行为：找不到 wbox-linux.exe 时 prepare 返回
//! "Linux 后端尚未就绪"的明确错误（而不是含糊的"文件不存在"）。

use super::{Backend, Prepared, RunSpec};
use crate::error::{ErrKind, Result, WboxError};
use std::path::{Path, PathBuf};

/// Linux 模拟器二进制文件名。
pub const LINUX_EXE_NAME: &str = "wbox-linux.exe";
/// 显式指定 wbox-linux.exe 路径的环境变量。
pub const LINUX_EXE_ENV: &str = "WBOX_LINUX";
/// blink VFS 根前缀（guest `/` 映射到的宿主目录），必须设置，
/// 否则动态链接的 guest 程序会命中宿主系统库（见 research/blink-validation）。
pub const BLINK_PREFIX_ENV: &str = "BLINK_PREFIX";

/// Linux 镜像后端（无状态）。
pub struct BlinkBackend;

/// 定位 wbox-linux.exe：`WBOX_LINUX` > wbox 自身 exe 同目录。
/// 返回 `(路径, 来源描述)`。仅做存在性检查，不校验可执行性。
fn locate_linux_exe() -> Result<(PathBuf, &'static str)> {
    if let Some(p) = std::env::var_os(LINUX_EXE_ENV) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok((p, "WBOX_LINUX 环境变量"));
        }
        return Err(not_ready_error(format!(
            "{} 指向的 '{}' 不存在",
            LINUX_EXE_ENV,
            p.display()
        )));
    }
    // wbox.exe 同目录（portable 分发形态：两个 exe 放在一起）
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join(LINUX_EXE_NAME);
            if p.is_file() {
                return Ok((p, "wbox 同目录"));
            }
        }
    }
    Err(not_ready_error(format!(
        "未找到 {}（请将 {} 与 wbox.exe 放在同一目录，或设置 {} 环境变量指向它）",
        LINUX_EXE_NAME, LINUX_EXE_NAME, LINUX_EXE_ENV
    )))
}

/// 统一的"后端未就绪"错误（退出码 4 = 进程创建类，语义最接近）。
fn not_ready_error(detail: String) -> WboxError {
    WboxError::new(
        ErrKind::Spawn,
        anyhow::anyhow!(
            "Linux 后端尚未就绪：{}。wbox-linux（blink Win32 移植）仍在开发中，\
             目前 `wbox run` 只能直接运行 Windows 原生程序",
            detail
        ),
    )
}

/// 构造 wbox-linux 的命令行：`wbox-linux.exe <guest cmd...>`。
/// （rootfs 不经命令行传递，而是工作目录 + BLINK_PREFIX 环境变量。）
fn build_blink_command(exe: &Path, guest_cmd: &[String]) -> Vec<String> {
    let mut cmd = vec![exe.to_string_lossy().into_owned()];
    cmd.extend(guest_cmd.iter().cloned());
    cmd
}

impl Backend for BlinkBackend {
    fn prepare(&self, spec: &RunSpec) -> Result<Prepared> {
        if spec.cmd.is_empty() {
            return Err(WboxError::args(
                "镜像未声明 Entrypoint/Cmd，请在 `--` 后显式给出要执行的命令",
            ));
        }
        let (exe, _src) = locate_linux_exe()?;
        let rootfs = &spec.workdir; // 镜像模式下 workdir = rootfs 目录
        if !rootfs.is_dir() {
            return Err(WboxError::registry(format!(
                "镜像 rootfs 目录 '{}' 不存在（是否已成功 pull？）",
                rootfs.display()
            )));
        }
        let mut env = spec.env.clone();
        // BLINK_PREFIX：guest `/` 的 VFS 根。用户已在 Env 里显式设置则不覆盖。
        if !env.iter().any(|(k, _)| k == BLINK_PREFIX_ENV) {
            env.push((
                BLINK_PREFIX_ENV.to_string(),
                rootfs.to_string_lossy().into_owned(),
            ));
        }
        Ok(Prepared {
            cmd: build_blink_command(&exe, &spec.cmd),
            workdir: rootfs.clone(),
            env,
        })
    }

    #[cfg(windows)]
    fn spawn(&self, spec: &RunSpec, prepared: &Prepared) -> Result<u32> {
        // 双层隔离：wbox-linux.exe 经 NativeBackend 在 AppContainer + Job 内启动。
        // rootfs 目录需已对 AppContainer SID 授权（pull 时由 acl.rs 完成，
        // 手工复制的缓存可运行 `icacls <rootfs> /grant "*S-1-15-2-1:(OI)(CI)(RX)" /T`）。
        super::native::spawn_native(
            spec,
            prepared,
            &format!("wbox-linux（blink 模拟器，BLINK_PREFIX={}）", prepared.workdir.display()),
        )
    }

    #[cfg(not(windows))]
    fn spawn(&self, _spec: &RunSpec, _prepared: &Prepared) -> Result<u32> {
        Err(WboxError::new(
            ErrKind::Spawn,
            anyhow::anyhow!("Linux 后端仅在 Windows 上可用（外层隔离为 AppContainer/Job Object）"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(cmd: &[&str]) -> RunSpec {
        RunSpec {
            name: "t".to_string(),
            limits: Default::default(),
            allow_network: false,
            keep_profile: false,
            workdir: std::env::temp_dir(), // 已存在的目录充当 rootfs
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            env: vec![("PATH".to_string(), "/usr/bin".to_string())],
            verbose: false,
        }
    }

    #[test]
    fn prepare_errors_when_exe_missing() {
        // WBOX_LINUX 指向不存在的路径：必须报"Linux 后端尚未就绪"
        std::env::set_var(LINUX_EXE_ENV, "/nonexistent/wbox-linux.exe");
        let err = BlinkBackend.prepare(&spec(&["bash"])).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("Linux 后端尚未就绪"), "{}", msg);
        std::env::remove_var(LINUX_EXE_ENV);
    }

    #[test]
    fn prepare_builds_command_and_blink_prefix() {
        // WBOX_LINUX 指向已存在文件（用 /bin/true 或当前 exe 代替 exe）
        let fake = std::env::current_exe().unwrap();
        std::env::set_var(LINUX_EXE_ENV, &fake);
        let p = BlinkBackend.prepare(&spec(&["bash", "-l"])).unwrap();
        assert_eq!(p.cmd[0], fake.to_string_lossy());
        assert_eq!(&p.cmd[1..], &["bash", "-l"]);
        // BLINK_PREFIX 注入且指向 rootfs
        assert!(p
            .env
            .iter()
            .any(|(k, v)| k == BLINK_PREFIX_ENV && v == &std::env::temp_dir().to_string_lossy()));
        // 镜像 Env 保留
        assert!(p.env.iter().any(|(k, _)| k == "PATH"));
        std::env::remove_var(LINUX_EXE_ENV);
    }

    #[test]
    fn user_blink_prefix_not_overridden() {
        let fake = std::env::current_exe().unwrap();
        std::env::set_var(LINUX_EXE_ENV, &fake);
        let mut s = spec(&["bash"]);
        s.env.push((BLINK_PREFIX_ENV.to_string(), "/custom".to_string()));
        let p = BlinkBackend.prepare(&s).unwrap();
        assert_eq!(
            p.env.iter().filter(|(k, _)| k == BLINK_PREFIX_ENV).count(),
            1
        );
        assert_eq!(
            p.env.iter().find(|(k, _)| k == BLINK_PREFIX_ENV).unwrap().1,
            "/custom"
        );
        std::env::remove_var(LINUX_EXE_ENV);
    }

    #[test]
    fn prepare_requires_command() {
        let err = BlinkBackend.prepare(&spec(&[])).unwrap_err();
        assert!(format!("{}", err).contains("Entrypoint/Cmd"));
    }
}
