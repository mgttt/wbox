//! `wbox pause` / `wbox unpause`：暂停与恢复容器（`PRD.md` F9.21）。
//!
//! # 用信号而不是 cgroup freezer，以及代价
//!
//! docker/podman 用 cgroup 的 freezer 冻结整组进程。wbox 不能照抄：容器的
//! cgroup **只在设了 `--memory`/`--cpu-pct`/`--max-procs` 时才存在**（见
//! `linux_limits.rs`），没设限额就没有可冻的组。让 `pause` 时灵时不灵，
//! 比换一种实现更糟。
//!
//! 所以这里对容器内**每个**进程发 `SIGSTOP`（`unpause` 发 `SIGCONT`），
//! 进程清单复用 `top` 的同一份枚举——一处另写一份的话，迟早出现
//! "top 看得见的进程 pause 漏了"这种最难查的偏差。
//!
//! **代价必须直说**：`SIGSTOP` 与 freezer 不等价。
//! - 进程能观察到自己被停过（`SIGCONT` 后 `wait` 会返回、有些程序会重试或报错），
//!   freezer 则对进程透明；
//! - 停的是发信号那一刻已存在的进程，其后新 fork 出来的不受影响——
//!   但父进程已停，正常情况下不会再有新进程。
//!
//! 对"临时腾出 CPU / 挂起一个跑长任务的容器"这类主要用途，这些差异无关紧要；
//! 对依赖信号语义的 guest 则可能有影响，故文档写明而不是假装等价。

use crate::error::{Result, WboxError};
use crate::runstate::{self, Liveness};

/// 暂停还是恢复。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Pause,
    Unpause,
}

impl Action {
    fn verb(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Unpause => "unpause",
        }
    }
}

/// 收**一个或多个**容器名，与 `kill`/`stop`/`rm` 一致。
///
/// 一批容器要一起暂停时，逐个敲命令中间会有可观的时间差；能一次给全，
/// 至少把这个差压到最小。
fn parse<'a>(args: &'a [String], verb: &str) -> Result<Vec<&'a str>> {
    super::args::take_container_names(args, verb)
}

pub fn cmd_pause(args: &[String]) -> Result<u32> {
    run(args, Action::Pause)
}

pub fn cmd_unpause(args: &[String]) -> Result<u32> {
    run(args, Action::Unpause)
}

fn run(args: &[String], action: Action) -> Result<u32> {
    let names = parse(args, action.verb())?;
    let owned: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    // 回显在 `apply` 里（Linux 分支成功时打名字），故这里 Echo::Nothing。
    super::args::each_named(&owned, action.verb(), super::args::Echo::Nothing, |name| {
        let dir = runstate::resolve_existing(name)?;
        // 已退出的容器没有可停的进程。明确报错而不是静默成功——静默成功会让
        // 脚本以为容器还在、还能 unpause 回来。
        if runstate::liveness(&dir) == Liveness::Exited {
            return Err(runstate::already_exited_for(name, action.verb()));
        }
        apply(name, &dir, action).map(|_| ())
    })
}

#[cfg(target_os = "linux")]
fn apply(name: &str, dir: &std::path::Path, action: Action) -> Result<u32> {
    let root = runstate::container_pid(dir).ok_or_else(|| {
        WboxError::args(format!(
            "容器 '{}' 尚未记录容器 PID（可能刚启动），请稍后重试 {}",
            name,
            action.verb()
        ))
    })?;
    let sig = match action {
        Action::Pause => libc::SIGSTOP,
        Action::Unpause => libc::SIGCONT,
    };
    let pids = super::top::container_pids(root);
    let mut hit = 0usize;
    for pid in &pids {
        // SAFETY: 只是给已枚举到的 pid 发信号；失败（进程刚退出）不致命。
        if unsafe { libc::kill(*pid as libc::pid_t, sig) } == 0 {
            hit += 1;
        }
    }
    if hit == 0 {
        return Err(WboxError::args(format!(
            "容器 '{}' 没有可{}的进程（可能刚退出）",
            name,
            if action == Action::Pause { "暂停" } else { "恢复" }
        )));
    }
    println!("{}", name);
    Ok(0)
}

#[cfg(not(target_os = "linux"))]
fn apply(name: &str, _dir: &std::path::Path, action: Action) -> Result<u32> {
    Err(WboxError::args(format!(
        "{}（{}）目前只在 Linux 宿主可用：它靠向容器内每个进程发信号实现，\
         Windows 侧的进程组语义不同，需另行设计（PRD F9.21）",
        action.verb(),
        name
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    /// 收一个或多个名字——与 `kill`/`stop`/`rm` 一致。一批容器要一起暂停时，
    /// 逐个敲命令中间会有可观的时间差。
    #[test]
    fn parse_takes_one_or_more_names() {
        assert_eq!(parse(&["c".to_string()], "pause").unwrap(), vec!["c"]);
        assert_eq!(
            parse(&["a".to_string(), "b".to_string()], "pause").unwrap(),
            vec!["a", "b"]
        );
        assert!(parse(&[], "pause").is_err());
        assert!(parse(&["-x".to_string()], "pause").is_err());
    }

    #[test]
    fn unknown_container_is_an_error() {
        let _home = TempHome::new("pauseunknown");
        assert!(cmd_pause(&["nope".to_string()]).is_err());
        assert!(cmd_unpause(&["nope".to_string()]).is_err());
    }

    /// 已退出的容器要报错。静默成功会让脚本以为它还在、还能 unpause 回来。
    #[test]
    fn exited_container_is_rejected() {
        let _home = TempHome::new("pauseexited");
        let dir = runstate::dir_for("dead").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lock"), b"").unwrap();
        std::fs::write(
            dir.join("meta.json"),
            r#"{"name":"dead","pid":1,"created_unix":1,"cmd":["x"],"target":"t"}"#,
        )
        .unwrap();
        let m = format!("{}", cmd_pause(&["dead".to_string()]).unwrap_err());
        assert!(m.contains("退出"), "{}", m);
    }
}
