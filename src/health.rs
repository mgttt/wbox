//! `--health-cmd`：容器健康检查（`PRD.md` F9.10）。
//!
//! # 谁来跑探针
//!
//! 与 `--restart` 同一个取舍（见 `restart.rs`）：探针循环放在 **supervisor
//! 自己**身上，不另起守护进程。好处一样——supervisor 一死，健康检查随之停止，
//! 不会出现"容器早没了、探针还在报 healthy"的僵尸状态；代价也一样——
//! supervisor 崩溃时健康检查随之失效。
//!
//! # 探针跑在容器里，不是宿主上
//!
//! 这条必须做对，否则整个功能是假的：探针经 `setns` 进容器的
//! user/mount/pid/net namespace 执行，复用 `wbox exec` 的同一条路径。
//! 若图省事在宿主上跑，探到的是宿主的状态，而用户以为探的是容器——
//! 这比没有健康检查危险得多。
//!
//! # 状态怎么存
//!
//! 单独一个 `health` 文件，不塞进 `meta.json`。理由是**写入频率**：探针按
//! interval 反复写，而 meta.json 的每次写都要取状态操作锁；让一个每 30 秒
//! 触发的后台循环去争那把锁，是拿正确性换一点存储上的整洁。

use crate::error::{Result, WboxError};
use std::time::Duration;

/// 状态文件名（位于容器状态目录内）。
pub const HEALTH_FILE: &str = "health";

/// 默认探测间隔。与 docker 一致。
const DEFAULT_INTERVAL: u64 = 30;
/// 默认连续失败多少次才判定 unhealthy。与 docker 一致。
const DEFAULT_RETRIES: u32 = 3;

/// 健康状态。语义与 docker 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    /// 还在 start period 内，或尚未探测成功过
    Starting,
    Healthy,
    Unhealthy,
}

impl HealthStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Healthy => "healthy",
            Self::Unhealthy => "unhealthy",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "starting" => Some(Self::Starting),
            "healthy" => Some(Self::Healthy),
            "unhealthy" => Some(Self::Unhealthy),
            _ => None,
        }
    }
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 健康检查配置。`None` 表示没开。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthSpec {
    /// 探针命令。整条交给容器内的 `/bin/sh -c` 执行，与 docker 的
    /// `CMD-SHELL` 形式一致——健康检查十有八九要用管道或 `&&`，
    /// 拆成 argv 反而是给用户添麻烦。
    pub cmd: String,
    pub interval: Duration,
    pub retries: u32,
    /// 起始宽限期：这段时间内探针失败**不计入** retries，也不会判 unhealthy。
    /// 慢启动的服务没有它就会在起来之前先被判死。
    pub start_period: Duration,
}

impl HealthSpec {
    pub fn new(cmd: String) -> Self {
        Self {
            cmd,
            interval: Duration::from_secs(DEFAULT_INTERVAL),
            retries: DEFAULT_RETRIES,
            start_period: Duration::ZERO,
        }
    }
}

/// 把秒数取值解析成 `Duration`，取值必须为正整数秒。
pub fn parse_secs(flag: &str, v: &str) -> Result<Duration> {
    let n = v.parse::<u64>().map_err(|_| {
        WboxError::args(format!("{} 取值 '{}' 不是合法秒数", flag, v))
    })?;
    Ok(Duration::from_secs(n))
}

/// 校验配置自洽。间隔为 0 会让探针变成忙循环，必须挡下。
pub fn validate(spec: &HealthSpec) -> Result<()> {
    if spec.cmd.trim().is_empty() {
        return Err(WboxError::args("--health-cmd 不能为空"));
    }
    if spec.interval.is_zero() {
        return Err(WboxError::args(
            "--health-interval 不能为 0：探针会变成忙循环，把 CPU 占满",
        ));
    }
    if spec.retries == 0 {
        return Err(WboxError::args(
            "--health-retries 不能为 0：那样第一次探测失败就判 unhealthy，\
             与 docker 语义不符（docker 最小为 1）",
        ));
    }
    Ok(())
}

/// `--health-cmd` 在**当前宿主**是否可用。
///
/// 探针要 `setns` 进容器，那是 Linux 的原语；Windows 侧 exec 走的是另一套
/// （同 Job + 同 SID 重建），尚未打通，故明确拒绝而不是静默不探。
pub fn reject_if_unsupported(spec: Option<&HealthSpec>) -> Result<()> {
    if spec.is_none() || cfg!(target_os = "linux") {
        return Ok(());
    }
    Err(WboxError::args(
        "--health-cmd 目前只在 Linux 宿主可用：探针要 setns 进容器的 namespace 执行，\
         Windows 侧尚未打通对应路径",
    ))
}

/// 写状态文件。写失败只是这一轮状态没更新，不该让 supervisor 挂掉。
///
/// 只有 Linux 的探针线程会写；其余宿主在 CLI 层就已拒绝 `--health-cmd`，
/// 故那些构建里没有写入方（读取方 `ps` 则是跨平台的）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn write_status(dir: &std::path::Path, s: HealthStatus) {
    let _ = std::fs::write(dir.join(HEALTH_FILE), s.as_str().as_bytes());
}

/// 读状态。没开健康检查时文件不存在，返回 `None`。
pub fn read_status(dir: &std::path::Path) -> Option<HealthStatus> {
    std::fs::read_to_string(dir.join(HEALTH_FILE))
        .ok()
        .and_then(|s| HealthStatus::parse(&s))
}

/// 起一个后台线程按 interval 探测，直到容器消失。
///
/// 线程是 daemon 性质的：supervisor 退出时进程整个结束，它自然停止——
/// 这正是我们要的语义（见模块文档）。
#[cfg(target_os = "linux")]
pub fn spawn_monitor(name: String, spec: HealthSpec) {
    std::thread::spawn(move || {
        let Ok(dir) = crate::runstate::dir_for(&name) else {
            return;
        };
        write_status(&dir, HealthStatus::Starting);
        let started = std::time::Instant::now();
        let mut fails = 0u32;
        loop {
            std::thread::sleep(spec.interval);
            // 容器没了就收工。以状态目录为准而不是以自身存活为准：
            // --restart 期间容器会短暂消失又回来，那不该终止监控。
            if !dir.exists() {
                return;
            }
            if crate::runstate::liveness(&dir) == crate::runstate::Liveness::Exited {
                return;
            }
            let Some(pid) = crate::runstate::container_pid(&dir) else {
                // 刚重启、container.pid 还没写回来：这不是探测失败，跳过这轮。
                continue;
            };
            let ok = probe(pid, &spec.cmd);
            let in_grace = started.elapsed() < spec.start_period;
            if ok {
                fails = 0;
                write_status(&dir, HealthStatus::Healthy);
            } else if in_grace {
                // 宽限期内的失败不计数、不改判定——慢启动的服务本就还没起来。
                write_status(&dir, HealthStatus::Starting);
            } else {
                fails += 1;
                if fails >= spec.retries {
                    write_status(&dir, HealthStatus::Unhealthy);
                }
            }
        }
    });
}

/// 在容器内跑一次探针，返回是否成功（退出码 0）。
///
/// 复用 `wbox exec` 的 setns 路径，**不是**在宿主上跑——探到宿主的状态而让
/// 用户以为探的是容器，比没有健康检查更糟。
#[cfg(target_os = "linux")]
fn probe(pid: u32, cmd: &str) -> bool {
    matches!(
        crate::cli::exec::exec_in_namespaces_quiet(pid, &["/bin/sh", "-c", cmd]),
        Ok(0)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_roundtrips_through_the_file() {
        let dir = std::env::temp_dir().join(format!("wbox-health-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_status(&dir), None, "没写过应为 None");
        for s in [
            HealthStatus::Starting,
            HealthStatus::Healthy,
            HealthStatus::Unhealthy,
        ] {
            write_status(&dir, s);
            assert_eq!(read_status(&dir), Some(s));
            assert_eq!(format!("{}", s), s.as_str());
        }
        std::fs::write(dir.join(HEALTH_FILE), b"garbage").unwrap();
        assert_eq!(read_status(&dir), None, "认不出的内容不该当成某个状态");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn defaults_match_docker() {
        let s = HealthSpec::new("true".to_string());
        assert_eq!(s.interval, Duration::from_secs(30));
        assert_eq!(s.retries, 3);
        assert_eq!(s.start_period, Duration::ZERO);
        assert!(validate(&s).is_ok());
    }

    /// interval=0 会让探针变忙循环；retries=0 会让第一次失败就判死。
    /// 两者都必须挡下并说清原因。
    #[test]
    fn rejects_self_defeating_settings() {
        let mut s = HealthSpec::new("true".to_string());
        s.interval = Duration::ZERO;
        let m = format!("{}", validate(&s).unwrap_err());
        assert!(m.contains("忙循环"), "{}", m);

        let mut s = HealthSpec::new("true".to_string());
        s.retries = 0;
        assert!(format!("{}", validate(&s).unwrap_err()).contains("docker"));

        assert!(validate(&HealthSpec::new("  ".to_string())).is_err());
    }

    #[test]
    fn parse_secs_rejects_junk() {
        assert_eq!(
            parse_secs("--health-interval", "5").unwrap(),
            Duration::from_secs(5)
        );
        assert!(parse_secs("--health-interval", "5s").is_err());
        assert!(parse_secs("--health-interval", "-1").is_err());
    }

    #[test]
    fn rejected_off_linux_only_when_configured() {
        assert!(reject_if_unsupported(None).is_ok());
        let s = HealthSpec::new("true".to_string());
        let r = reject_if_unsupported(Some(&s));
        if cfg!(target_os = "linux") {
            assert!(r.is_ok());
        } else {
            let m = format!("{}", r.unwrap_err());
            assert!(m.contains("只在 Linux"), "{}", m);
            assert!(m.contains("setns"), "要说清为什么做不到：{}", m);
        }
    }
}
