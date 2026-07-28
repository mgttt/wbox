//! `wbox stop`：停掉运行中的容器（`PRD.md` F8.3）。
//!
//! 停的是 **supervisor（wbox 自己）**，不是 guest。容器整棵进程树的存活绑在
//! supervisor 上（Linux 的 PDEATHSIG、Windows 的 Job kill-on-close），
//! supervisor 一死内核就收走整棵树；直接去杀 guest 反而会漏掉它的子孙。

use crate::error::{Result, WboxError};
use crate::runstate::{self, Kill, Liveness};

/// 等待容器退出的默认上限（秒）。超时后升级为强制终止。
pub(super) const DEFAULT_TIMEOUT_SECS: u64 = 10;

struct StopOptions<'a> {
    name: &'a str,
    timeout: u64,
}

/// `stop` 的命令行：一个 `--timeout` 加**一个或多个**容器名。
///
/// 收多个是补出来的：`kill`/`rm`/`start`/`wait` 早就收多个，唯独 `stop` 只收一个
/// ——同一批容器能一起 kill 却不能一起 stop，是纯粹的不一致，用户没法从别的命令
/// 推断出这条例外。
fn parse_many(args: &[String]) -> Result<(Vec<&str>, u64)> {
    let mut names = Vec::new();
    let mut timeout = DEFAULT_TIMEOUT_SECS;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-t" | "--timeout" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| WboxError::args("stop: --timeout 缺少取值（秒）"))?;
                timeout = v.parse::<u64>().map_err(|_| {
                    WboxError::args(format!("stop: --timeout 取值 '{}' 不是合法秒数", v))
                })?;
            }
            other if other.starts_with('-') => {
                return Err(WboxError::args(format!(
                    "stop: 未知参数 '{}'（用法：wbox stop [--timeout <秒>] <NAME>...）",
                    other
                )))
            }
            other => names.push(other),
        }
        i += 1;
    }
    if names.is_empty() {
        return Err(WboxError::args(
            "stop: 缺少容器名（用法：wbox stop [--timeout <秒>] <NAME>...）",
        ));
    }
    Ok((names, timeout))
}

pub fn cmd_stop(args: &[String]) -> Result<u32> {
    let (names, timeout) = parse_many(args)?;
    let owned: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    // 回显交给 `stop_with` 自己（它还要区分"已经是停止状态"那种情形），
    // 所以这里用 Echo::Nothing——否则同一个名字会打两遍。
    super::args::each_named(&owned, "停止", super::args::Echo::Nothing, |name| {
        stop_with(&StopOptions { name, timeout }, Say::Name).map(|_| ())
    })
}

/// 停容器时要不要把容器名打出来。
///
/// `wbox stop` 打（脚本靠它确认停的是哪个）；`restart` 不打——它自己会在整轮
/// 结束时按批次汇报，让 stop 也打一遍会变成一个名字出现两次。
/// 用一个开关而不是给 restart 另写一条停止路径：另写的那条迟早与这条出现
/// 行为差异（先礼后兵的时序、Windows 的 Job 处理），而那种差异最难查。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Say {
    Name,
    Nothing,
}

pub(super) fn stop_quiet(name: &str, timeout: u64) -> Result<u32> {
    stop_with(&StopOptions { name, timeout }, Say::Nothing)
}

fn stop_with(opts: &StopOptions, say: Say) -> Result<u32> {
    let announce = |text: String| {
        if say == Say::Name {
            println!("{}", text);
        }
    };
    let locked = runstate::lock_existing(opts.name)?;
    let dir = locked.dir.clone();
    if runstate::liveness(&dir) == Liveness::Created {
        return Err(WboxError::args(format!(
            "容器 '{}' 尚未启动（状态为 created）",
            opts.name
        )));
    }
    if runstate::liveness(&dir) == Liveness::Exited {
        // 已经停了不算错：stop 的意图是"让它别再跑"，这个状态已经满足。
        // 报错只会让 `wbox stop x || true` 这类脚本写得别扭。
        drop(locked);
        announce(format!("{}（已经是停止状态）", opts.name));
        return Ok(0);
    }

    let pid = locked.entry.pid;

    #[cfg(unix)]
    {
        // 先礼：请它自己退出，让 guest 有机会跑完清理。
        runstate::terminate_pid(pid, Kill::Graceful);
        // Registration::drop 同样要取得操作锁后才能释放 owner 锁，等待前必须放锁。
        drop(locked);
        if wait_exit(&dir, opts.timeout) {
            announce(opts.name.to_string());
            return Ok(0);
        }

        // 后兵：超时未退则强制终止。
        runstate::terminate_pid(pid, Kill::Forceful);
        if wait_exit(&dir, 5) {
            eprintln!(
                "wbox: 容器 '{}' 未在 {} 秒内自行退出，已强制终止",
                opts.name, opts.timeout
            );
            announce(opts.name.to_string());
            return Ok(0);
        }
        Err(WboxError::args(format!(
            "容器 '{}' 强制终止后仍未退出（pid={}）",
            opts.name, pid
        )))
    }

    #[cfg(windows)]
    {
        let mut locked = locked;
        locked.mark_stopping()?;
        // 必须直接终止命名 Job。只杀 supervisor 并依赖 KILL_ON_JOB_CLOSE 不够：
        // 并发 exec 控制器可能仍持有同一 Job 的另一个 handle。
        let job =
            match crate::job::Job::wait_for_container(opts.name, std::time::Duration::from_secs(2))
            {
                Ok(job) => job,
                Err(open_error) => {
                    // 主进程可能已退出并销毁 Job、但 Registration 正在等我们释放
                    // operation lock 才能标记 Exited。先放锁再复核，避免把正常退出报错。
                    drop(locked);
                    if wait_exit(&dir, 1) {
                        announce(opts.name.to_string());
                        return Ok(0);
                    }
                    return Err(open_error);
                }
            };
        job.terminate(1)?;
        drop(locked);
        if wait_exit(&dir, opts.timeout) {
            announce(opts.name.to_string());
            return Ok(0);
        }

        // Job 已终止而 supervisor 仍未收尾时，只终止 supervisor 本身；Job 树在
        // 上一步已经显式清空，不依赖这个 handle 的关闭副作用。
        runstate::terminate_pid(pid, Kill::Forceful);
        if wait_exit(&dir, 5) {
            eprintln!(
                "wbox: 容器 '{}' 的 Job 已终止，supervisor 未在 {} 秒内退出，已强制终止",
                opts.name, opts.timeout
            );
            announce(opts.name.to_string());
            return Ok(0);
        }
        Err(WboxError::args(format!(
            "容器 '{}' 的 Job 与 supervisor 强制终止后仍未退出（pid={}）",
            opts.name, pid
        )))
    }
}

/// 轮询锁直到容器退出。**以锁为准而不是以 pid 为准**：pid 会被复用，
/// 而锁被释放才真正等价于"持有它的那个 wbox 没了"。
pub(crate) fn wait_exit(dir: &std::path::Path, secs: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        if runstate::liveness(dir) == Liveness::Exited {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn parse_reads_names_and_timeout() {
        let a = ["c1".to_string()];
        let (names, timeout) = parse_many(&a).unwrap();
        assert_eq!(names, vec!["c1"]);
        assert_eq!(timeout, DEFAULT_TIMEOUT_SECS);

        let b = ["c1".to_string(), "--timeout".to_string(), "3".to_string()];
        let (names, timeout) = parse_many(&b).unwrap();
        assert_eq!(names, vec!["c1"]);
        assert_eq!(timeout, 3);

        // 多个名字要收下——`kill`/`rm` 早就收多个，stop 不该是例外
        let c = ["a".to_string(), "b".to_string()];
        assert_eq!(parse_many(&c).unwrap().0, vec!["a", "b"]);

        assert!(parse_many(&[]).is_err());
        assert!(
            parse_many(&["--timeout".to_string()]).is_err(),
            "缺取值应报错"
        );
        assert!(
            parse_many(&["c".to_string(), "-t".to_string(), "x".to_string()]).is_err(),
            "非数字应报错"
        );
    }

    /// 停一个已经停了的容器不该报错——否则 `wbox stop x` 在脚本里没法幂等使用。
    #[test]
    fn stopping_exited_container_is_not_an_error() {
        let _home = TempHome::new("already");
        let dir = runstate::dir_for("dead").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lock"), b"").unwrap();
        std::fs::write(
            dir.join("meta.json"),
            r#"{"name":"dead","pid":1,"created_unix":1,"cmd":["x"],"target":"t"}"#,
        )
        .unwrap();
        assert_eq!(cmd_stop(&["dead".to_string()]).unwrap(), 0);
    }

    #[test]
    fn unknown_container_is_an_error() {
        let _home = TempHome::new("unknown");
        assert!(cmd_stop(&["nope".to_string()]).is_err());
    }

    #[test]
    fn name_cannot_escape_run_root() {
        let _home = TempHome::new("escape");
        assert!(cmd_stop(&["../evil".to_string()]).is_err());
    }
}
