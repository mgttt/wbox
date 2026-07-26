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
use crate::error::{Result, WboxError};

/// Linux 原生容器后端（无状态）。
pub struct LinuxNativeBackend;

impl Backend for LinuxNativeBackend {
    fn prepare(&self, spec: &RunSpec) -> Result<Prepared> {
        super::require_cmd(&spec.cmd)?;
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
        if spec.verbose {
            super::verbose_kv("宿主后端", "linux-native（L0 骨架：仅执行计划）");
            super::verbose_kv("rootfs", rootfs.display());
            super::verbose_kv("guest 命令行（Entrypoint/Cmd 合并后）", format!("{:?}", cmd));
        }
        Ok(Prepared {
            cmd,
            workdir: rootfs.clone(),
            env,
        })
    }

    fn spawn(&self, _spec: &RunSpec, _prepared: &Prepared) -> Result<u32> {
        // L1 才落地隔离。这里刻意报错而非"直接 exec 了事"——
        // 无隔离地执行容器命令会让"同一条 wbox run 在两个宿主上隔离强度不同"，
        // 正是 §10.5「语义一致性红线」明令禁止的行为。
        Err(WboxError::spawn(
            "Linux 原生后端尚未就绪：执行计划已可构造（L0），但 user/mount/pid \
             namespace 与 cgroup v2 限额（L1/L2）未实现。在隔离落地前拒绝执行，\
             以免与 Windows 侧的隔离承诺不一致；详见 docs-architecture.md §10.5",
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
        let p = LinuxNativeBackend.prepare(&s).unwrap();
        // 与 Blink 的关键差异：命令行就是 guest 命令本身，前面不插模拟器
        assert_eq!(p.cmd, vec!["/bin/sh", "-l"]);
        assert_eq!(p.workdir, rootfs);
        let _ = std::fs::remove_dir_all(&rootfs);
    }

    #[test]
    fn prepare_injects_resolv_conf() {
        let rootfs = temp_rootfs("resolv");
        let s = spec(&rootfs, &["/bin/true"], vec![]);
        LinuxNativeBackend.prepare(&s).unwrap();
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
        let p = LinuxNativeBackend.prepare(&s).unwrap();
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
        let p = LinuxNativeBackend.prepare(&s).unwrap();
        assert!(p.env.iter().any(|(k, v)| k == "APP_TOKEN" && v == "hunter2"));
        assert!(!p.env.iter().any(|(k, _)| k == "WBOX_VA_BITS"));
        assert!(!p.env.iter().any(|(k, _)| k == "BLINK_PREFIX"));
        let _ = std::fs::remove_dir_all(&rootfs);
    }

    #[test]
    fn prepare_rejects_empty_cmd_and_missing_rootfs() {
        let rootfs = temp_rootfs("reject");
        assert!(LinuxNativeBackend.prepare(&spec(&rootfs, &[], vec![])).is_err());
        let missing = rootfs.join("nope");
        assert!(LinuxNativeBackend
            .prepare(&spec(&missing, &["/bin/true"], vec![]))
            .is_err());
        let _ = std::fs::remove_dir_all(&rootfs);
    }

    /// L0 阶段 spawn 必须**明确拒绝**而不是无隔离执行（§10.5 语义一致性红线）。
    #[test]
    fn spawn_refuses_until_isolation_lands() {
        let rootfs = temp_rootfs("spawn");
        let s = spec(&rootfs, &["/bin/true"], vec![]);
        let p = LinuxNativeBackend.prepare(&s).unwrap();
        let err = LinuxNativeBackend.spawn(&s, &p).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("尚未就绪"), "{}", msg);
        let _ = std::fs::remove_dir_all(&rootfs);
    }
}
