//! `wbox kill`：立即终止容器，不经过 `stop` 的优雅退出等待。

use crate::error::{Result, WboxError};
use crate::runstate::{self, Kill, Liveness};

struct KillOptions<'a> {
    names: Vec<&'a str>,
}

fn parse<'a>(args: &'a [String]) -> Result<KillOptions<'a>> {
    let mut names = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-s" | "--signal" => {
                i += 1;
                let signal = args
                    .get(i)
                    .ok_or_else(|| WboxError::args("kill: --signal 缺少取值"))?;
                if !matches!(
                    signal.to_ascii_uppercase().as_str(),
                    "KILL" | "SIGKILL" | "9"
                ) {
                    return Err(WboxError::args(format!(
                        "kill: 当前跨平台仅支持 KILL/SIGKILL/9，收到 '{}'",
                        signal
                    )));
                }
            }
            other if other.starts_with('-') => {
                return Err(WboxError::args(format!(
                    "kill: 未知参数 '{}'（用法：wbox kill [-s KILL] <NAME>...）",
                    other
                )))
            }
            other => names.push(other),
        }
        i += 1;
    }
    if names.is_empty() {
        return Err(WboxError::args(
            "kill: 缺少容器名（用法：wbox kill [-s KILL] <NAME>...）",
        ));
    }
    Ok(KillOptions { names })
}

fn kill_one(name: &str) -> Result<()> {
    let locked = runstate::lock_existing(name)?;
    let dir = locked.dir.clone();
    if runstate::liveness(&dir) == Liveness::Exited {
        return Err(WboxError::args(format!("容器 '{}' 已退出", name)));
    }
    let pid = locked.entry.pid;

    #[cfg(unix)]
    {
        runstate::terminate_pid(pid, Kill::Forceful);
        drop(locked);
    }

    #[cfg(windows)]
    {
        let mut locked = locked;
        locked.mark_stopping()?;
        let job = match crate::job::Job::wait_for_container(name, std::time::Duration::from_secs(2))
        {
            Ok(job) => job,
            Err(open_error) => {
                drop(locked);
                if stop_waited(&dir, 1) {
                    return Ok(());
                }
                return Err(open_error);
            }
        };
        job.terminate(137)?;
        runstate::terminate_pid(pid, Kill::Forceful);
        drop(locked);
    }

    if stop_waited(&dir, 5) {
        Ok(())
    } else {
        Err(WboxError::args(format!(
            "容器 '{}' 强制终止后仍未退出（pid={}）",
            name, pid
        )))
    }
}

fn stop_waited(dir: &std::path::Path, secs: u64) -> bool {
    crate::cli::stop::wait_exit(dir, secs)
}

pub fn cmd_kill(args: &[String]) -> Result<u32> {
    let opts = parse(args)?;
    for name in opts.names {
        kill_one(name)?;
        println!("{}", name);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn parse_accepts_docker_signal_spellings_and_multiple_names() {
        for signal in ["KILL", "SIGKILL", "9", "kill"] {
            let args = vec![
                "-s".to_string(),
                signal.to_string(),
                "a".to_string(),
                "b".to_string(),
            ];
            assert_eq!(parse(&args).unwrap().names, vec!["a", "b"]);
        }
        assert!(parse(&["-s".to_string(), "TERM".to_string(), "a".to_string()]).is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn unknown_and_exited_containers_are_errors() {
        let _home = TempHome::new("kill-errors");
        assert!(cmd_kill(&["missing".to_string()]).is_err());

        let dir = runstate::dir_for("dead").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lock"), b"").unwrap();
        std::fs::write(
            dir.join("meta.json"),
            r#"{"name":"dead","pid":1,"created_unix":1,"cmd":["x"],"target":"t"}"#,
        )
        .unwrap();
        assert!(cmd_kill(&["dead".to_string()]).is_err());
    }
}
