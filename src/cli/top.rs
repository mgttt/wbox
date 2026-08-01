//! `wbox top`：列出隔离单元中的进程，而不是 wbox supervisor。

use crate::error::{Result, WboxError};
use crate::runstate::{self, Liveness};

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessRow {
    pid: u32,
    command: String,
}

fn parse(args: &[String]) -> Result<&str> {
    if args.len() != 1 || args[0].starts_with('-') {
        return Err(WboxError::args(
            "top: 当前用法为 wbox top <NAME>（暂不支持追加 ps 参数）",
        ));
    }
    Ok(&args[0])
}

pub fn cmd_top(args: &[String]) -> Result<u32> {
    let name = parse(args)?;
    let locked = runstate::lock_existing(name)?;
    if runstate::liveness(&locked.dir) == Liveness::Created {
        return Err(WboxError::args(format!(
            "容器 '{}' 尚未启动（状态为 created）",
            name
        )));
    }
    if runstate::liveness(&locked.dir) == Liveness::Exited {
        return Err(WboxError::args(format!("容器 '{}' 已退出", name)));
    }
    let mut rows = platform_rows(name, &locked.dir)?;
    rows.sort_by_key(|row| row.pid);
    println!("{:<10} COMMAND", "PID");
    for row in rows {
        println!("{:<10} {}", row.pid, row.command);
    }
    Ok(0)
}

#[cfg(windows)]
fn platform_rows(name: &str, _dir: &std::path::Path) -> Result<Vec<ProcessRow>> {
    let job = crate::job::Job::wait_for_container(name, std::time::Duration::from_secs(2))?;
    Ok(job
        .process_ids()?
        .into_iter()
        .map(|pid| ProcessRow {
            pid,
            command: process_image_name(pid),
        })
        .collect())
}

#[cfg(windows)]
fn process_image_name(pid: u32) -> String {
    agenterm_platform::process_image::executable_path(pid)
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "-".to_owned())
}

#[cfg(target_os = "linux")]
fn platform_rows(_name: &str, dir: &std::path::Path) -> Result<Vec<ProcessRow>> {
    let root = runstate::container_pid(dir)
        .ok_or_else(|| WboxError::args("容器刚启动，尚未记录容器 PID；请稍后重试 top"))?;
    Ok(linux_process_tree(root))
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform_rows(_name: &str, _dir: &std::path::Path) -> Result<Vec<ProcessRow>> {
    Err(WboxError::spawn("top 尚未实现当前宿主的进程枚举后端"))
}

/// 容器内进程的宿主 PID 清单（含 root 本身）。
///
/// `top` 与 `pause`/`unpause` 共用同一份枚举：一处按 PPID 闭包展开、
/// 一处另写一份的话，迟早出现"top 看得见的进程 pause 漏了"这种最难查的偏差。
#[cfg(target_os = "linux")]
pub(super) fn container_pids(root: u32) -> Vec<u32> {
    use std::collections::HashSet;
    let mut processes = Vec::<(u32, u32)>::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return vec![root];
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if let Some((ppid, _)) = linux_process(pid) {
            processes.push((pid, ppid));
        }
    }
    let mut members = HashSet::from([root]);
    loop {
        let before = members.len();
        for &(pid, ppid) in &processes {
            if members.contains(&ppid) {
                members.insert(pid);
            }
        }
        if members.len() == before {
            break;
        }
    }
    let mut out: Vec<u32> = members.into_iter().collect();
    out.sort_unstable();
    out
}

/// `/proc/<pid>/stat` 的状态字符（`R`/`S`/`T`/`Z`…）。进程没了返回 `None`。
///
/// **必须从最后一个 `)` 之后开始切**：comm 字段里可能有空格和括号，
/// 按空格切会整体错位，读到的"状态"其实是别的字段。
#[cfg(target_os = "linux")]
pub(super) fn proc_state(pid: u32) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.rfind(')')?;
    stat.get(close + 2..)?
        .split_whitespace()
        .next()?
        .chars()
        .next()
}

#[cfg(target_os = "linux")]
fn linux_process_tree(root: u32) -> Vec<ProcessRow> {
    use std::collections::{HashMap, HashSet};

    let mut processes = HashMap::<u32, (u32, String)>::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if let Some((ppid, command)) = linux_process(pid) {
            processes.insert(pid, (ppid, command));
        }
    }

    let mut members = HashSet::from([root]);
    loop {
        let before = members.len();
        for (&pid, &(ppid, _)) in &processes {
            if members.contains(&ppid) {
                members.insert(pid);
            }
        }
        if members.len() == before {
            break;
        }
    }

    let mut rows = members
        .into_iter()
        .filter_map(|pid| {
            processes.get(&pid).map(|(_, command)| ProcessRow {
                pid,
                command: command.clone(),
            })
        })
        .collect::<Vec<_>>();
    // Linux PID namespace 的直接宿主子进程是等待 guest 的中间进程；存在真正
    // guest 时不把这个实现细节暴露给用户。
    if rows.len() > 1 {
        rows.retain(|row| row.pid != root);
    }
    rows
}

#[cfg(target_os = "linux")]
fn linux_process(pid: u32) -> Option<(u32, String)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.rfind(')')?;
    let fields = stat
        .get(close + 2..)?
        .split_whitespace()
        .collect::<Vec<_>>();
    let ppid = fields.get(1)?.parse().ok()?;
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let command = if cmdline.is_empty() {
        stat.get(stat.find('(')? + 1..close)?.to_string()
    } else {
        String::from_utf8_lossy(&cmdline)
            .split('\0')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };
    Some((ppid, command))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn parse_requires_exactly_one_name() {
        assert_eq!(parse(&["box".to_string()]).unwrap(), "box");
        assert!(parse(&[]).is_err());
        assert!(parse(&["box".to_string(), "aux".to_string()]).is_err());
        assert!(parse(&["--help".to_string()]).is_err());
    }

    #[test]
    fn unknown_and_exited_containers_are_errors() {
        let _home = TempHome::new("top-errors");
        assert!(cmd_top(&["missing".to_string()]).is_err());

        let dir = runstate::dir_for("dead").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lock"), b"").unwrap();
        std::fs::write(
            dir.join("meta.json"),
            r#"{"name":"dead","pid":1,"created_unix":1,"cmd":["x"],"target":"t"}"#,
        )
        .unwrap();
        assert!(cmd_top(&["dead".to_string()]).is_err());
    }

    /// 本进程当然是在跑的（`R` 或 `S`），不该被读成 `T`（暂停）。
    /// 这条同时钉住字段偏移：comm 含空格时按空格切会读到别的字段。
    #[cfg(target_os = "linux")]
    #[test]
    fn proc_state_reads_a_running_process() {
        let st = proc_state(std::process::id()).unwrap();
        assert!(matches!(st, 'R' | 'S'), "本进程状态应是 R/S，得到 {}", st);
        assert_eq!(proc_state(u32::MAX), None, "不存在的 pid 应返回 None");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_parser_reads_this_process() {
        let (ppid, command) = linux_process(std::process::id()).unwrap();
        assert!(ppid > 0);
        assert!(!command.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn process_image_name_uses_platform_lookup_and_product_fallback() {
        assert_ne!(process_image_name(std::process::id()), "-");
        assert_eq!(process_image_name(u32::MAX), "-");
    }
}
