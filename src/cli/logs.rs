//! `wbox logs`：读取 `--detach` 容器的输出（`PRD.md` F8.2）。
//!
//! 只读命令。日志由 supervisor 的 stdio 直接落盘，本命令只负责把文件读出来，
//! 因此对**运行中和已退出**的容器都能用——排查失败容器时后者尤其要紧。

use crate::error::{Result, WboxError};
use crate::runstate;
use std::io::Read;

struct LogsOptions<'a> {
    name: &'a str,
    /// 读 stderr 而不是 stdout
    stderr: bool,
}

fn parse<'a>(args: &'a [String]) -> Result<LogsOptions<'a>> {
    let mut name: Option<&str> = None;
    let mut stderr = false;
    for a in args {
        match a.as_str() {
            "--stderr" | "-e" => stderr = true,
            other if other.starts_with('-') => {
                return Err(WboxError::args(format!(
                    "logs: 未知参数 '{}'（用法：wbox logs <NAME> [--stderr]）",
                    other
                )))
            }
            other => {
                if name.is_some() {
                    return Err(WboxError::args("logs: 一次只能读一个容器"));
                }
                name = Some(other);
            }
        }
    }
    let name = name.ok_or_else(|| {
        WboxError::args("logs: 缺少容器名（用法：wbox logs <NAME> [--stderr]）")
    })?;
    Ok(LogsOptions { name, stderr })
}

pub fn cmd_logs(args: &[String]) -> Result<u32> {
    let opts = parse(args)?;
    let dir = runstate::dir_for(opts.name)?;
    if !dir.exists() {
        return Err(WboxError::args(format!(
            "没有名为 '{}' 的容器记录",
            opts.name
        )));
    }
    let file = if opts.stderr {
        runstate::LOG_STDERR
    } else {
        runstate::LOG_STDOUT
    };
    let path = dir.join(file);
    if !path.exists() {
        // 前台运行的容器不落盘日志——这不是错误，但必须说清楚，否则用户会
        // 以为容器没有输出。
        return Err(WboxError::args(format!(
            "容器 '{}' 没有 {} 日志（只有 --detach 启动的容器才落盘）",
            opts.name, file
        )));
    }
    let mut buf = Vec::new();
    std::fs::File::open(&path)
        .and_then(|mut f| f.read_to_end(&mut buf))
        .map_err(|e| WboxError::args(format!("读取 '{}' 失败：{}", path.display(), e)))?;
    // 按字节原样写出：guest 的输出未必是合法 UTF-8，用 String 转换会破坏它
    use std::io::Write;
    std::io::stdout()
        .write_all(&buf)
        .map_err(|e| WboxError::args(format!("写出日志失败：{}", e)))?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::EnvGuard;
    use std::path::PathBuf;

    fn tmp_home(tag: &str) -> (PathBuf, EnvGuard) {
        let d = std::env::temp_dir().join(format!("wbox-logs-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut g = EnvGuard::new();
        g.set("HOME", d.to_str().unwrap());
        g.set("USERPROFILE", d.to_str().unwrap());
        (d, g)
    }

    #[test]
    fn parse_accepts_name_and_stderr_flag() {
        let one = ["c1".to_string()];
        let o = parse(&one).unwrap();
        assert_eq!(o.name, "c1");
        assert!(!o.stderr);
        assert!(parse(&["c1".to_string(), "--stderr".to_string()]).unwrap().stderr);
        assert!(parse(&[]).is_err(), "缺名字应报错");
        assert!(parse(&["a".to_string(), "b".to_string()]).is_err(), "两个名字应报错");
        assert!(parse(&["--bogus".to_string()]).is_err());
    }

    #[test]
    fn reads_stdout_log() {
        let (home, _g) = tmp_home("read");
        let dir = runstate::dir_for("c").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(runstate::LOG_STDOUT), b"hello\n").unwrap();
        assert_eq!(cmd_logs(&["c".to_string()]).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// 前台容器没有日志文件——要给出可懂的解释，不能让用户以为"没输出"。
    #[test]
    fn missing_log_explains_detach_requirement() {
        let (home, _g) = tmp_home("nolog");
        let dir = runstate::dir_for("fg").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let err = cmd_logs(&["fg".to_string()]).unwrap_err();
        assert!(format!("{}", err).contains("--detach"), "{}", err);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unknown_container_is_an_error() {
        let (home, _g) = tmp_home("unknown");
        assert!(cmd_logs(&["nope".to_string()]).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn name_cannot_escape_run_root() {
        let (home, _g) = tmp_home("escape");
        assert!(cmd_logs(&["../evil".to_string()]).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }
}
