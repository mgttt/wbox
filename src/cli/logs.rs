//! `wbox logs`：读取 `--detach` 容器的输出（`PRD.md` F8.2、F9.28）。
//!
//! 只读命令。日志由 supervisor 的 stdio 直接落盘，本命令只负责把文件读出来，
//! 因此对**运行中和已退出**的容器都能用——排查失败容器时后者尤其要紧。
//!
//! # `--follow` 要认日志被截断
//!
//! 日志有体积上限，超了会被**截断到 0** 再从头写（见 `runstate::enforce_log_cap`）。
//! 跟随时若只记一个"读到哪了"的偏移量，截断之后文件长度会小于该偏移，
//! 于是再也读不到新内容——命令看着还在跟，其实已经哑了。**静默降级是最糟的
//! 失败形态**，所以这里显式检测"文件变短了"并把偏移归零。

use crate::error::{Result, WboxError};
use crate::runstate::{self, Liveness};
use std::io::{Read, Seek, SeekFrom, Write};

/// 跟随时的轮询间隔。够快到像实时，又不至于空转烧 CPU。
const POLL: std::time::Duration = std::time::Duration::from_millis(200);

#[derive(Debug)]
struct LogsOptions<'a> {
    name: &'a str,
    /// 读 stderr 而不是 stdout
    stderr: bool,
    /// 持续跟随新输出
    follow: bool,
    /// 只看最后 N 行；`None` = 全部
    tail: Option<usize>,
}

fn parse<'a>(args: &'a [String]) -> Result<LogsOptions<'a>> {
    let mut name: Option<&str> = None;
    let mut stderr = false;
    let mut follow = false;
    let mut tail = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--stderr" | "-e" => stderr = true,
            "--follow" | "-f" => follow = true,
            "--tail" | "-n" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| WboxError::args("logs: --tail 缺少取值（行数）"))?;
                // `all` 是 docker 的写法，等价于不限行数
                tail = if v == "all" {
                    None
                } else {
                    Some(v.parse::<usize>().map_err(|_| {
                        WboxError::args(format!("logs: --tail 取值 '{}' 不是合法行数", v))
                    })?)
                };
            }
            other if other.starts_with('-') => {
                return Err(WboxError::args(format!(
                    "logs: 未知参数 '{}'（用法：wbox logs [-f] [--tail N] [--stderr] <NAME>）",
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
        i += 1;
    }
    let name = name.ok_or_else(|| {
        WboxError::args("logs: 缺少容器名（用法：wbox logs [-f] [--tail N] [--stderr] <NAME>）")
    })?;
    Ok(LogsOptions {
        name,
        stderr,
        follow,
        tail,
    })
}

/// 取 `data` 的最后 `n` 行（保留每行的换行符）。
///
/// 末尾那次分割产生的空段不算一行：日志通常以 `\n` 结尾，把它算成一行的话
/// `--tail 1` 会只输出一个空行。
pub fn last_lines(data: &[u8], n: usize) -> &[u8] {
    if n == 0 {
        return &[];
    }
    let end = if data.ends_with(b"\n") {
        data.len() - 1
    } else {
        data.len()
    };
    let mut seen = 0;
    let mut idx = end;
    while idx > 0 {
        if data[idx - 1] == b'\n' {
            seen += 1;
            if seen == n {
                return &data[idx..];
            }
        }
        idx -= 1;
    }
    data
}

/// 从 `offset` 起读到文件末尾，返回读到的字节与新的偏移。
///
/// **文件比偏移还短就当作被截断**（日志超上限会被清零重写），此时从头读起。
/// 不认这一下的话，截断之后跟随会永远读不到东西——命令看着还在跟，其实已经哑了。
fn read_from(path: &std::path::Path, offset: u64) -> std::io::Result<(Vec<u8>, u64)> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = if len < offset { 0 } else { offset };
    if len == start {
        return Ok((Vec::new(), start));
    }
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let next = start + buf.len() as u64;
    Ok((buf, next))
}

pub fn cmd_logs(args: &[String]) -> Result<u32> {
    let opts = parse(args)?;
    let dir = runstate::resolve_existing(opts.name)?;
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

    let read_err =
        |e: std::io::Error| WboxError::args(format!("读取 '{}' 失败：{}", path.display(), e));
    let (buf, mut offset) = read_from(&path, 0).map_err(read_err)?;
    let shown = match opts.tail {
        Some(n) => last_lines(&buf, n),
        None => &buf[..],
    };
    // 按字节原样写出：guest 的输出未必是合法 UTF-8，用 String 转换会破坏它
    let mut out = std::io::stdout();
    out.write_all(shown)
        .map_err(|e| WboxError::args(format!("写出日志失败：{}", e)))?;
    let _ = out.flush();
    if !opts.follow {
        return Ok(0);
    }

    loop {
        // 先判活、再读一次：顺序反过来会丢掉容器在"读完"与"判活"之间写下的
        // 最后一段输出——而那一段往往正是失败原因。
        let running = runstate::liveness(&dir) == Liveness::Running;
        let (chunk, next) = read_from(&path, offset).map_err(read_err)?;
        offset = next;
        if !chunk.is_empty() {
            out.write_all(&chunk)
                .map_err(|e| WboxError::args(format!("写出日志失败：{}", e)))?;
            let _ = out.flush();
        }
        if !running {
            // 容器已退出且刚才那次读已经把剩下的都取走了，跟随结束。
            return Ok(0);
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn parse_accepts_name_and_flags() {
        let one = ["c1".to_string()];
        let o = parse(&one).unwrap();
        assert_eq!(o.name, "c1");
        assert!(!o.stderr);
        assert!(!o.follow);
        assert_eq!(o.tail, None);
        assert!(
            parse(&["c1".to_string(), "--stderr".to_string()])
                .unwrap()
                .stderr
        );
        assert!(parse(&["-f".to_string(), "c1".to_string()]).unwrap().follow);
        let a: Vec<String> = ["--tail", "5", "c1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(parse(&a).unwrap().tail, Some(5));
        // docker 的 `--tail all` 等价于不限行数
        let a: Vec<String> = ["--tail", "all", "c1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(parse(&a).unwrap().tail, None);
        assert!(parse(&[]).is_err(), "缺名字应报错");
        assert!(
            parse(&["a".to_string(), "b".to_string()]).is_err(),
            "两个名字应报错"
        );
        assert!(parse(&["--bogus".to_string()]).is_err());
        assert!(parse(&["--tail".to_string()]).is_err(), "缺取值应报错");
        assert!(parse(&["--tail".to_string(), "x".to_string()]).is_err());
    }

    /// 末尾的 `\n` 不该被当成一整行，否则 `--tail 1` 只输出一个空行。
    #[test]
    fn last_lines_counts_real_lines_not_the_trailing_newline() {
        assert_eq!(last_lines(b"a\nb\nc\n", 1), b"c\n");
        assert_eq!(last_lines(b"a\nb\nc\n", 2), b"b\nc\n");
        // 要的行数比实际多 → 给全部，而不是报错或给空
        assert_eq!(last_lines(b"a\nb\n", 9), b"a\nb\n");
        // 没有结尾换行符的情况
        assert_eq!(last_lines(b"a\nb", 1), b"b");
        assert_eq!(last_lines(b"", 3), b"");
        assert_eq!(last_lines(b"a\n", 0), b"");
    }

    /// 日志超上限会被截断到 0 再从头写。跟随时若不认这一下，
    /// 之后就永远读不到新内容——命令看着还在跟，其实已经哑了。
    #[test]
    fn read_from_restarts_when_the_file_was_truncated() {
        let base = std::env::temp_dir().join(format!("wbox-logs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let p = base.join("out.log");

        std::fs::write(&p, b"first\nsecond\n").unwrap();
        let (b1, off1) = read_from(&p, 0).unwrap();
        assert_eq!(b1, b"first\nsecond\n");
        // 没有新内容时读到空，偏移不动
        let (b2, off2) = read_from(&p, off1).unwrap();
        assert!(b2.is_empty());
        assert_eq!(off2, off1);
        // 追加 → 只读到新增那段
        std::fs::write(&p, b"first\nsecond\nthird\n").unwrap();
        let (b3, off3) = read_from(&p, off2).unwrap();
        assert_eq!(b3, b"third\n");
        // 截断重写 → 必须从头读，而不是因为 len < offset 就再也读不到
        std::fs::write(&p, b"tiny\n").unwrap();
        let (b4, off4) = read_from(&p, off3).unwrap();
        assert_eq!(b4, b"tiny\n", "截断后要从头读");
        assert_eq!(off4, 5);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reads_stdout_log() {
        let _home = TempHome::new("read");
        let dir = runstate::dir_for("c").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(runstate::LOG_STDOUT), b"hello\n").unwrap();
        assert_eq!(cmd_logs(&["c".to_string()]).unwrap(), 0);
        // 已退出的容器加 -f 也应立刻返回（没有可等的东西），不能挂住
        assert_eq!(
            cmd_logs(&["-f".to_string(), "c".to_string()]).unwrap(),
            0,
            "对已退出容器跟随必须立即结束"
        );
    }

    /// 前台容器没有日志文件——要给出可懂的解释，不能让用户以为"没输出"。
    #[test]
    fn missing_log_explains_detach_requirement() {
        let _home = TempHome::new("nolog");
        let dir = runstate::dir_for("fg").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let err = cmd_logs(&["fg".to_string()]).unwrap_err();
        assert!(format!("{}", err).contains("--detach"), "{}", err);
    }

    #[test]
    fn unknown_container_is_an_error() {
        let _home = TempHome::new("unknown");
        assert!(cmd_logs(&["nope".to_string()]).is_err());
    }

    #[test]
    fn name_cannot_escape_run_root() {
        let _home = TempHome::new("escape");
        assert!(cmd_logs(&["../evil".to_string()]).is_err());
    }
}
