//! `wbox prune`：批量清掉已退出的容器记录（`PRD.md` F9.27）。
//!
//! # 为什么需要它
//!
//! `--detach` 的容器退出后**刻意保留**记录与日志（用户回头查 `wbox logs` 正是
//! 为了看它输出了什么）。代价是这些记录会累积，久了 `ps -a` 里全是历史残渣。
//! docker 的答案是 `container prune`，这里照做。
//!
//! # 默认要先问
//!
//! 删除不可逆，而 `prune` 一次删一批。默认先列出将要删的、要求 `-f` 确认——
//! 交互式确认在这里不合适：wbox 常在脚本和 CI 里跑，读 stdin 会挂住。
//! 所以是"两步走"而不是"y/N"：先看清单，确认无误再加 `-f`。
//!
//! # 运行中的一个都不碰
//!
//! 判据取自 [`crate::runstate::liveness`]（与 `ps` 同一份），不另写一套判活。

use crate::error::{Result, WboxError};
use crate::runstate::{self, Liveness};

#[derive(Debug, Default)]
struct Options {
    force: bool,
}

fn parse(args: &[String]) -> Result<Options> {
    let mut o = Options::default();
    for a in args {
        match a.as_str() {
            "-f" | "--force" => o.force = true,
            other => {
                return Err(WboxError::args(format!(
                    "prune: 未知参数 '{}'（用法：wbox prune [-f]）",
                    other
                )))
            }
        }
    }
    Ok(o)
}

/// 可清理的容器名（已退出的；`created` 从没跑过，不属于"残渣"）。
///
/// `created` 不清是刻意的：那是用户 `wbox create` 特意留着待 start 的配置，
/// 把它当垃圾扫掉会让人白白丢失参数。docker 的 `container prune` 同样只清
/// 已停止的。
fn prunable() -> Result<Vec<String>> {
    Ok(runstate::list()?
        .into_iter()
        .filter(|(_, live)| *live == Liveness::Exited)
        .map(|(e, _)| e.name)
        .collect())
}

pub fn cmd_prune(args: &[String]) -> Result<u32> {
    let o = parse(args)?;
    let mut names = prunable()?;
    names.sort();
    if names.is_empty() {
        println!("没有可清理的已退出容器记录");
        return Ok(0);
    }
    if !o.force {
        println!("将删除以下 {} 条已退出的容器记录（含它们的日志）：", names.len());
        for n in &names {
            println!("  {}", n);
        }
        println!("确认无误后加 -f 执行：wbox prune -f");
        // 返回 0：这是一次成功的"预演"，不是失败。让 `wbox prune || echo 出错`
        // 这类写法不至于误报。
        return Ok(0);
    }
    // 逐个删、逐个报，不一遇错就中断（与 `rm` 同一取舍）：中断会让用户以为
    // 一条都没删掉，而实际上前面几条已经删了。
    let mut failed = 0usize;
    for n in &names {
        match runstate::remove(n) {
            Ok(()) => println!("{}", n),
            Err(e) => {
                eprintln!("wbox: 删除 '{}' 失败：{}", n, e);
                failed += 1;
            }
        }
    }
    if failed > 0 {
        return Err(WboxError::args(format!(
            "{} 条记录未能删除（共 {} 条）",
            failed,
            names.len()
        )));
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn parse_only_accepts_force() {
        assert!(!parse(&[]).unwrap().force);
        assert!(parse(&["-f".to_string()]).unwrap().force);
        assert!(parse(&["--force".to_string()]).unwrap().force);
        assert!(parse(&["-a".to_string()]).is_err());
    }

    /// 没有 `-f` 时**一条都不能删**。默认就删的话，一次手误就没了一批记录。
    #[test]
    fn dry_run_lists_without_deleting() {
        let _home = TempHome::new("prunedry");
        let dir = runstate::dir_for("gone").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("meta.json"),
            r#"{"name":"gone","pid":1,"created_unix":1,"cmd":["x"],"target":"t"}"#,
        )
        .unwrap();

        assert_eq!(cmd_prune(&[]).unwrap(), 0, "预演是成功而不是失败");
        assert!(dir.exists(), "没有 -f 时不该删任何东西");

        assert_eq!(cmd_prune(&["-f".to_string()]).unwrap(), 0);
        assert!(!dir.exists(), "-f 之后才真的删");
    }

    /// `created` 是用户特意留着待 start 的配置，不是残渣，扫掉会让人白丢参数。
    #[test]
    fn created_containers_are_not_prunable() {
        let _home = TempHome::new("prunecreated");
        let dir = runstate::dir_for("pending").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), r#"{"name":"pending","pid":0,"created_unix":1,"cmd":["x"],"target":"t"}"#).unwrap();
        std::fs::write(dir.join(".created"), b"").unwrap();
        assert_eq!(runstate::liveness(&dir), Liveness::Created);
        assert!(
            !prunable().unwrap().contains(&"pending".to_string()),
            "created 容器不该被 prune 掉"
        );
        assert_eq!(cmd_prune(&["-f".to_string()]).unwrap(), 0);
        assert!(dir.exists(), "created 容器必须原样留着");
    }

    #[test]
    fn empty_state_is_reported_not_an_error() {
        let _home = TempHome::new("pruneempty");
        assert_eq!(cmd_prune(&["-f".to_string()]).unwrap(), 0);
    }
}
