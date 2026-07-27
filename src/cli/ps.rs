//! `wbox ps`：列出已登记的容器（`PRD.md` F8.1）。
//!
//! 只读命令，不碰任何容器。存活判定见 `crate::runstate` 的模块文档——
//! 靠锁文件而非 pid，因此不会把复用了 pid 的无关进程认成自己的容器。

use crate::error::Result;
use crate::runstate::{self, Liveness};

/// 解析 `wbox ps` 的参数。目前只有 `-a/--all`。
#[derive(Debug)]
struct PsOptions {
    /// 默认只列运行中的；`-a` 连已退出的残留一并列出
    all: bool,
}

fn parse(args: &[String]) -> Result<PsOptions> {
    let mut o = PsOptions { all: false };
    for a in args {
        match a.as_str() {
            "-a" | "--all" => o.all = true,
            other => {
                return Err(crate::error::WboxError::args(format!(
                    "ps: 未知参数 '{}'（支持 -a/--all）",
                    other
                )))
            }
        }
    }
    Ok(o)
}

pub fn cmd_ps(args: &[String]) -> Result<u32> {
    let opts = parse(args)?;
    let all = runstate::list()?;
    let rows: Vec<_> = all
        .iter()
        .filter(|(_, l)| opts.all || *l == Liveness::Running)
        .collect();

    // 空表也要给一行说明，而不是什么都不打印——"没输出"分不清是"没有容器"
    // 还是"命令没跑起来"。
    if rows.is_empty() {
        if all.is_empty() {
            println!("没有已登记的容器。");
        } else {
            println!("没有运行中的容器（有 {} 条非运行记录，用 -a 查看）。", all.len());
        }
        return Ok(0);
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("{:<20} {:<20} {:<8} {:<10} 命令", "名称", "状态", "PID", "已运行");
    for (e, l) in rows {
        let base = match l {
            Liveness::Created => "created",
            Liveness::Running => "running",
            Liveness::Exited => "exited",
        };
        // 健康状态并进"状态"列而不是新增一列（docker 也是这么显示的）：
        // 没开健康检查的容器占多数，为它们凭空加一列空白不划算。
        let state = match crate::runstate::dir_for(&e.name)
            .ok()
            .and_then(|d| crate::health::read_status(&d))
        {
            Some(h) if *l == Liveness::Running => format!("{} ({})", base, h),
            _ => base.to_string(),
        };
        // 已退出的条目算"存活时长"没有意义，且时钟回拨会得到负数——一律显示 "-"
        let age = if *l == Liveness::Running && now >= e.created_unix {
            human_age(now - e.created_unix)
        } else {
            "-".to_string()
        };
        println!(
            "{:<20} {:<20} {:<8} {:<10} {}",
            truncate(&e.name, 20),
            state,
            e.pid,
            age,
            truncate(&e.cmd.join(" "), 48)
        );
    }
    Ok(0)
}

/// 秒 → 人读时长。不引 chrono：只是给人看个大概。
fn human_age(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{}s", s),
        s if s < 3600 => format!("{}m{}s", s / 60, s % 60),
        s if s < 86400 => format!("{}h{}m", s / 3600, (s % 3600) / 60),
        s => format!("{}d{}h", s / 86400, (s % 86400) / 3600),
    }
}

/// 按**字符**截断（非字节）：命令行里有中文时按字节切会切出乱码。
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_all_flag_and_rejects_junk() {
        assert!(!parse(&[]).unwrap().all);
        assert!(parse(&["-a".to_string()]).unwrap().all);
        assert!(parse(&["--all".to_string()]).unwrap().all);
        let e = parse(&["--bogus".to_string()]).unwrap_err();
        assert!(format!("{}", e).contains("未知参数"));
    }

    #[test]
    fn human_age_covers_each_unit() {
        assert_eq!(human_age(0), "0s");
        assert_eq!(human_age(59), "59s");
        assert_eq!(human_age(61), "1m1s");
        assert_eq!(human_age(3661), "1h1m");
        assert_eq!(human_age(90061), "1d1h");
    }

    /// 截断必须按字符：按字节切中文会切出半个字符。
    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abcdef", 4), "abc…");
        let cn = "运行一个很长的中文命令行";
        let t = truncate(cn, 5);
        assert_eq!(t.chars().count(), 5, "{}", t);
        // 能被正常解码即说明没切坏
        assert!(t.ends_with('…'));
    }
}
