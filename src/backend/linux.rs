//! `LinuxNativeBackend`：Linux 宿主上直接运行 Linux 容器（多宿主扩展 L0 骨架）。
//!
//! 设计与里程碑见 `docs-architecture.md` §10.2 / §10.5。本文件对应 **L0**：
//! 只做 `prepare`（构造执行计划，不启动进程），使镜像解析 → config 合并 →
//! 环境策略这条链路在 Linux 宿主上完整跑通并可单测；隔离本身（user/mount/pid
//! namespace、`pivot_root`、cgroup v2）属 L1/L2，`spawn` 目前明确报未实现。
//!
//! 与 `BlinkBackend` 的关键差异：
//! - 不需要模拟器——宿主就是 Linux，guest ELF 直接执行，故命令行即 `spec.cmd`
//!   本身（Blink 侧要在前面插 `wbox-linux.exe`）；
//! - 不注入 `BLINK_PREFIX`——rootfs 由 L1 的 `pivot_root` 真正成为 `/`，
//!   而不是靠模拟器的 VFS 前缀；
//! - 环境白名单用 POSIX 风味（见 `env::GuestFlavor`），不注入 `SystemRoot`。
//!
//! 复用的既有资产：`oci` 整个模块（拉取/解包/config 合并）、`backend::env`
//! 的保留键剥离与脱敏策略、`require_cmd`/`verbose_kv` 等共享原语——这正是
//! §10.2 判断"Linux 后端复用面最大、风险最低"的依据。

use super::{Backend, Prepared, RunSpec};

#[cfg(target_os = "linux")]
#[path = "linux_ns.rs"]
mod ns;
use crate::error::{Result, WboxError};

/// 运行形态：镜像容器 vs 宿主程序。
///
/// 两者共用同一套隔离原语（namespace + cgroup），差别只在**根文件系统**：
/// - `Image`：`pivot_root` 进镜像 rootfs，容器看到的是镜像的 `/`；
/// - `Host`：**不** `pivot_root`，宿主文件系统照常可见，`--workdir` 只作
///   工作目录。这与 Windows 侧的语义一致（README「不提供什么」已写明
///   `--workdir` 不是只读视图），故不是 Linux 侧的特殊约定。
///   用途是"环境控制"而非"文件系统隔离"：资源限额、进程树隔离、
///   默认断网——给 harness / CI 跑宿主已有程序时用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxMode {
    /// OCI 镜像 rootfs（pivot_root 进去）
    Image,
    /// 宿主程序（不换根）
    Host,
}

/// Linux 原生后端。`mode` 决定是否 `pivot_root` 进镜像 rootfs。
pub struct LinuxNativeBackend(pub LinuxMode);

impl Backend for LinuxNativeBackend {
    fn prepare(&self, spec: &RunSpec) -> Result<Prepared> {
        super::require_cmd(&spec.cmd)?;
        if self.0 == LinuxMode::Host {
            // 宿主模式：不换根，故不校验 rootfs、不注入 resolv.conf
            // （用宿主自己的 /etc/resolv.conf）。workdir 就是工作目录。
            // 同下：wine 分支在非 Linux 目标下整块 cfg 掉，mut 是条件性需要的
            #[allow(unused_mut)]
            let mut env = super::build_sanitized_env(
                &spec.env,
                &[],
                spec.env_pass_all,
                spec.verbose,
                super::env::GuestFlavor::Linux,
            );
            // 台阶③：目标若是 PE，插一个 wine 加载器在前面。隔离原语一律不变
            // ——wine 只是执行器变体，`--memory`/`--max-procs`/`--allow-network`
            // 走的仍是同一条实现，不会出现"wine 那条路忘了限额"的分叉。
            // 非 Linux 目标下这两个绑定不会被改写（wine 分支整块 cfg 掉），
            // 故 mut 是条件性需要的
            #[allow(unused_mut)]
            let mut cmd = spec.cmd.clone();
            #[allow(unused_mut)]
            let mut via_wine: Option<(std::path::PathBuf, std::path::PathBuf)> = None;
            // wine 模块只在 Linux 编译：Windows 宿主跑 PE 走的是原生
            // AppContainer 路径，压根不该出现 wine。
            #[cfg(target_os = "linux")]
            if super::wine::is_pe(std::path::Path::new(&cmd[0])) {
                let wine = super::wine::find_wine(
                    std::env::var("WBOX_WINE").ok().as_deref(),
                    std::env::var("PATH").ok().as_deref(),
                )?;
                let prefix = super::wine::prepare_prefix()?;
                super::wine::augment_env(&mut env, &prefix, spec.verbose);
                cmd.insert(0, wine.display().to_string());
                via_wine = Some((wine, prefix));
            }
            if spec.verbose {
                super::verbose_kv("宿主后端", "linux-native（宿主程序模式，不换根）");
                super::verbose_kv("工作目录", spec.workdir.display());
                match &via_wine {
                    Some((w, p)) => {
                        #[cfg(target_os = "linux")]
                        let ver = super::wine::version(w).unwrap_or_else(|| "版本未知".to_string());
                        #[cfg(not(target_os = "linux"))]
                        let ver = String::new();
                        // 版本要打出来：wine 版本差异是 Windows 程序行为差异的
                        // 头号来源，而那类差异不属 wbox 缺陷（§10.3）。
                        super::verbose_kv(
                            "执行器",
                            format!("wine（目标是 PE）：{}［{}］", w.display(), ver),
                        );
                        super::verbose_kv("WINEPREFIX", p.display());
                    }
                    None => super::verbose_kv("执行器", "直接执行（目标是本机 ELF）"),
                }
            }
            return Ok(Prepared {
                cmd,
                workdir: spec.workdir.clone(),
                env,
            });
        }
        let rootfs = &spec.workdir; // 镜像模式下 workdir = rootfs 目录
        if !rootfs.is_dir() {
            return Err(WboxError::registry(format!(
                "镜像 rootfs 目录 '{}' 不存在（是否已成功 pull？）",
                rootfs.display()
            )));
        }
        // guest 程序要能解析域名：与 Blink 侧同一处理（缺失/空则注入公共 DNS）。
        // L1 的 mount namespace 落地后，这里写入的 resolv.conf 会随 rootfs
        // 一起成为容器内的 /etc/resolv.conf。
        if super::ensure_resolv_conf(rootfs)? && spec.verbose {
            super::verbose_kv(
                "resolv.conf",
                format!("rootfs 缺失/为空，已注入公共 DNS {}", super::blink::DEFAULT_DNS),
            );
        }
        // 环境：POSIX 风味白名单 + 镜像 Env（保留键已剥离）+ 强制项。
        // L0 无强制项——BLINK_PREFIX 是模拟器专用，Linux 原生不需要。
        let env =
            super::build_sanitized_env(&spec.env, &[], spec.env_pass_all, spec.verbose, super::env::GuestFlavor::Linux);
        // 宿主即 Linux，guest 命令直接就是最终命令行（无模拟器前缀）。
        let cmd = spec.cmd.clone();
        // 镜像模式下的 PE 暂未接线（台阶③ 目前只覆盖宿主模式）。这里必须**明确
        // 拒绝**：不拦的话 `execvp` 遇到 ENOEXEC 会按 POSIX 回退去用 /bin/sh
        // 解释这个文件，于是 busybox sh 把 PE 当 shell 脚本读，吐出
        // "MZ....: not found" 加一串语法错误——用户完全看不出发生了什么
        // （实测如此，rc=2）。宁可给一句能懂的话。
        #[cfg(target_os = "linux")]
        {
            let guest = cmd[0].strip_prefix('/').unwrap_or(&cmd[0]);
            if super::wine::is_pe(&rootfs.join(guest)) {
                return Err(WboxError::args(format!(
                    "'{}' 是 Windows 程序（PE），但镜像模式暂不支持经 wine 执行。\
                     目前台阶③ 只覆盖宿主模式：`wbox run -- <你的.exe>`（不带镜像引用）。\
                     不拦下来的话，容器内的 shell 会把这个 PE 当脚本解释，报错难以理解",
                    cmd[0]
                )));
            }
        }
        if spec.verbose {
            super::verbose_kv("宿主后端", "linux-native（镜像模式，pivot_root 进 rootfs）");
            super::verbose_kv("rootfs", rootfs.display());
            super::verbose_kv("guest 命令行（Entrypoint/Cmd 合并后）", format!("{:?}", cmd));
        }
        Ok(Prepared {
            cmd,
            workdir: rootfs.clone(),
            env,
        })
    }

    #[cfg(target_os = "linux")]
    fn spawn(&self, spec: &RunSpec, prepared: &Prepared) -> Result<u32> {
        ns::spawn_isolated(spec, prepared, self.0)
    }

    #[cfg(not(target_os = "linux"))]
    fn spawn(&self, _spec: &RunSpec, _prepared: &Prepared) -> Result<u32> {
        Err(WboxError::spawn(
            "Linux 原生后端只能在 Linux 宿主上执行（当前宿主请用 BlinkBackend）",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Limits;
    use std::path::{Path, PathBuf};

    /// 造一个最小 rootfs 目录，返回路径（调用方负责清理）。
    fn temp_rootfs(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wbox-linuxbe-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        dir
    }

    fn spec(rootfs: &Path, cmd: &[&str], env: Vec<(String, String)>) -> RunSpec {
        RunSpec {
            name: "t".to_string(),
            limits: Limits::default(),
            allow_network: false,
            keep_profile: false,
            workdir: rootfs.to_path_buf(),
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            env,
            verbose: false,
            env_pass_all: false,
        }
    }

    #[test]
    fn prepare_builds_plan_without_emulator_prefix() {
        let rootfs = temp_rootfs("plan");
        let s = spec(&rootfs, &["/bin/sh", "-l"], vec![]);
        let p = LinuxNativeBackend(LinuxMode::Image).prepare(&s).unwrap();
        // 与 Blink 的关键差异：命令行就是 guest 命令本身，前面不插模拟器
        assert_eq!(p.cmd, vec!["/bin/sh", "-l"]);
        assert_eq!(p.workdir, rootfs);
        let _ = std::fs::remove_dir_all(&rootfs);
    }

    #[test]
    fn prepare_injects_resolv_conf() {
        let rootfs = temp_rootfs("resolv");
        let s = spec(&rootfs, &["/bin/true"], vec![]);
        LinuxNativeBackend(LinuxMode::Image).prepare(&s).unwrap();
        let resolv = std::fs::read_to_string(rootfs.join("etc/resolv.conf")).unwrap();
        assert!(resolv.contains("nameserver"), "{}", resolv);
        let _ = std::fs::remove_dir_all(&rootfs);
    }

    /// POSIX 风味不得注入 Windows 键——这是 Linux 后端与 Blink 后端
    /// 在环境策略上的分水岭，回归了会把 `SystemRoot=C:\Windows` 塞进容器。
    #[test]
    fn prepare_env_is_posix_flavored() {
        let rootfs = temp_rootfs("env");
        let s = spec(&rootfs, &["/bin/true"], vec![]);
        let p = LinuxNativeBackend(LinuxMode::Image).prepare(&s).unwrap();
        assert!(
            !p.env.iter().any(|(k, _)| k.eq_ignore_ascii_case("SystemRoot")),
            "Linux 容器不应出现 SystemRoot：{:?}",
            p.env
        );
        assert!(
            !p.env.iter().any(|(k, _)| k.eq_ignore_ascii_case("COMSPEC")),
            "Linux 容器不应出现 COMSPEC：{:?}",
            p.env
        );
        let _ = std::fs::remove_dir_all(&rootfs);
    }

    /// 镜像 Env 照常注入，保留键（WBOX_*/BLINK_*）照常剥离——
    /// 与 Blink/Native 共用同一出口，此处确认 Linux 后端没绕过它。
    #[test]
    fn prepare_applies_shared_env_policy() {
        let rootfs = temp_rootfs("policy");
        let s = spec(
            &rootfs,
            &["/bin/true"],
            vec![
                ("APP_TOKEN".to_string(), "hunter2".to_string()),
                ("WBOX_VA_BITS".to_string(), "43".to_string()),
                ("BLINK_PREFIX".to_string(), "/evil".to_string()),
            ],
        );
        let p = LinuxNativeBackend(LinuxMode::Image).prepare(&s).unwrap();
        assert!(p.env.iter().any(|(k, v)| k == "APP_TOKEN" && v == "hunter2"));
        assert!(!p.env.iter().any(|(k, _)| k == "WBOX_VA_BITS"));
        assert!(!p.env.iter().any(|(k, _)| k == "BLINK_PREFIX"));
        let _ = std::fs::remove_dir_all(&rootfs);
    }

    #[test]
    fn prepare_rejects_empty_cmd_and_missing_rootfs() {
        let rootfs = temp_rootfs("reject");
        assert!(LinuxNativeBackend(LinuxMode::Image).prepare(&spec(&rootfs, &[], vec![])).is_err());
        let missing = rootfs.join("nope");
        assert!(LinuxNativeBackend(LinuxMode::Image)
            .prepare(&spec(&missing, &["/bin/true"], vec![]))
            .is_err());
        let _ = std::fs::remove_dir_all(&rootfs);
    }

    /// 命令不存在时应给出可读错误（而非 panic 或静默 rc）。
    #[cfg(target_os = "linux")]
    #[test]
    fn spawn_reports_missing_command() {
        let rootfs = temp_rootfs("nocmd");
        let s = spec(&rootfs, &["/bin/definitely-not-here"], vec![]);
        let p = LinuxNativeBackend(LinuxMode::Image).prepare(&s).unwrap();
        let err = LinuxNativeBackend(LinuxMode::Image).spawn(&s, &p).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("启动容器进程"), "{}", msg);
        let _ = std::fs::remove_dir_all(&rootfs);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn spawn_rejects_non_linux_host() {
        let rootfs = temp_rootfs("wrong-host");
        let s = spec(&rootfs, &["/bin/true"], vec![]);
        let p = LinuxNativeBackend(LinuxMode::Image).prepare(&s).unwrap();
        let err = LinuxNativeBackend(LinuxMode::Image).spawn(&s, &p).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("只能在 Linux 宿主上执行"), "{}", msg);
        let _ = std::fs::remove_dir_all(&rootfs);
    }

    /// L1 端到端：在 user+mount+pid namespace 内真跑一个静态 busybox，
    /// 用**退出码**断言 uid 映射生效（`id -u` 在容器内应为 0）。
    ///
    /// 前置：仓库根存在静态 busybox（矩阵已在用），且宿主允许 unprivileged
    /// user namespace。任一不满足按仓库惯例 SKIP 不 fail——环境能力缺失
    /// 不等于代码回归（docs/testing.md §一.1 的网络用例同一原则）。
    #[cfg(target_os = "linux")]
    #[test]
    fn spawn_maps_uid_to_root_in_namespace_or_skip() {
        let bb = std::path::Path::new("busybox");
        if !bb.is_file() {
            eprintln!("SKIP：仓库根无静态 busybox（矩阵用二进制），跳过 L1 端到端");
            return;
        }
        let rootfs = temp_rootfs("l1e2e");
        std::fs::copy(bb, rootfs.join("bin/busybox")).unwrap();
        // busybox 以 argv[0] 分派 applet，故用符号链接暴露 sh / id
        for a in ["sh", "id"] {
            let _ = std::os::unix::fs::symlink("busybox", rootfs.join("bin").join(a));
        }
        let s = spec(
            &rootfs,
            &["/bin/sh", "-c", "[ \"$(/bin/id -u)\" = 0 ]"],
            vec![],
        );
        let p = LinuxNativeBackend(LinuxMode::Image).prepare(&s).unwrap();
        match LinuxNativeBackend(LinuxMode::Image).spawn(&s, &p) {
            Ok(0) => {} // 容器内 id -u == 0：uid 映射生效
            Ok(rc) => panic!("容器内 id -u 不为 0（rc={}），uid 映射未生效", rc),
            Err(e) => {
                // 例如 CI 禁用 unprivileged userns：报 EPERM
                eprintln!("SKIP：无法建立 user namespace（{}）", e);
            }
        }
        let _ = std::fs::remove_dir_all(&rootfs);
    }
}
