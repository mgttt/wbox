//! `wbox stop`：停掉运行中的容器（`PRD.md` F8.3）。
//!
//! 停的是 **supervisor（wbox 自己）**，不是 guest。容器整棵进程树的存活绑在
//! supervisor 上（Linux 的 PDEATHSIG、Windows 的 Job kill-on-close），
//! supervisor 一死内核就收走整棵树；直接去杀 guest 反而会漏掉它的子孙。

use crate::error::{Result, WboxError};
use crate::runstate::{self, Kill, Liveness};

/// 等待容器退出的默认上限（秒）。超时后升级为强制终止。
const DEFAULT_TIMEOUT_SECS: u64 = 10;

struct StopOptions<'a> {
    name: &'a str,
    timeout: u64,
}

fn parse<'a>(args: &'a [String]) -> Result<StopOptions<'a>> {
    let mut name: Option<&str> = None;
    let mut timeout = DEFAULT_TIMEOUT_SECS;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-t" | "--timeout" => {
                i += 1;
                let v = args.get(i).ok_or_else(|| {
                    WboxError::args("stop: --timeout 缺少取值（秒）")
                })?;
                timeout = v.parse::<u64>().map_err(|_| {
                    WboxError::args(format!("stop: --timeout 取值 '{}' 不是合法秒数", v))
                })?;
            }
            other if other.starts_with('-') => {
                return Err(WboxError::args(format!(
                    "stop: 未知参数 '{}'（用法：wbox stop <NAME> [--timeout <秒>]）",
                    other
                )))
            }
            other => {
                if name.is_some() {
                    return Err(WboxError::args("stop: 一次只能停一个容器"));
                }
                name = Some(other);
            }
        }
        i += 1;
    }
    let name = name.ok_or_else(|| {
        WboxError::args("stop: 缺少容器名（用法：wbox stop <NAME> [--timeout <秒>]）")
    })?;
    Ok(StopOptions { name, timeout })
}

pub fn cmd_stop(args: &[String]) -> Result<u32> {
    let opts = parse(args)?;
    let dir = runstate::resolve_existing(opts.name)?;
    if runstate::liveness(&dir) == Liveness::Exited {
        // 已经停了不算错：stop 的意图是"让它别再跑"，这个状态已经满足。
        // 报错只会让 `wbox stop x || true` 这类脚本写得别扭。
        println!("{}（已经是停止状态）", opts.name);
        return Ok(0);
    }

    let pid = runstate::read_meta(&dir).map(|e| e.pid).ok_or_else(|| {
        WboxError::args(format!(
            "容器 '{}' 的 meta.json 不可读，无法确定要停的进程",
            opts.name
        ))
    })?;

    // 先礼：请它自己退出，让 guest 有机会跑完清理。
    // （Windows 没有 SIGTERM 的等价物，那里这一步等同于强制终止。）
    runstate::terminate_pid(pid, Kill::Graceful);
    if wait_exit(&dir, opts.timeout) {
        println!("{}", opts.name);
        return Ok(0);
    }

    // 后兵：超时未退则强制终止。
    runstate::terminate_pid(pid, Kill::Forceful);
    if wait_exit(&dir, 5) {
        eprintln!(
            "wbox: 容器 '{}' 未在 {} 秒内自行退出，已强制终止",
            opts.name, opts.timeout
        );
        println!("{}", opts.name);
        return Ok(0);
    }
    Err(WboxError::args(format!(
        "容器 '{}' 强制终止后仍未退出（pid={}）",
        opts.name, pid
    )))
}

/// 轮询锁直到容器退出。**以锁为准而不是以 pid 为准**：pid 会被复用，
/// 而锁被释放才真正等价于"持有它的那个 wbox 没了"。
fn wait_exit(dir: &std::path::Path, secs: u64) -> bool {
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
    fn parse_reads_name_and_timeout() {
        let a = ["c1".to_string()];
        assert_eq!(parse(&a).unwrap().timeout, DEFAULT_TIMEOUT_SECS);
        let b = ["c1".to_string(), "--timeout".to_string(), "3".to_string()];
        let o = parse(&b).unwrap();
        assert_eq!(o.name, "c1");
        assert_eq!(o.timeout, 3);
        assert!(parse(&[]).is_err());
        assert!(parse(&["--timeout".to_string()]).is_err(), "缺取值应报错");
        assert!(
            parse(&["c".to_string(), "-t".to_string(), "x".to_string()]).is_err(),
            "非数字应报错"
        );
        assert!(parse(&["a".to_string(), "b".to_string()]).is_err());
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
