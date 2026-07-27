//! `wbox restart`：停掉再按原配置起来（`PRD.md` F9.26）。
//!
//! # 纯编排，没有新机制
//!
//! `stop` 与 `start` 各自已经把难的部分做完了（前者停的是 supervisor 而不是
//! guest，后者在操作锁内原子领取配置）。这里只做两件事：按顺序调它们，
//! 以及处理"本来就没在跑"这种情形。**不另写一条启停路径**——另写的那条迟早
//! 与 `stop`/`start` 出现行为差异，而这种差异最难查。
//!
//! # 已经停了的容器照样能 restart
//!
//! docker 是这个语义，理由也站得住：用户想要的是"让它按原配置重新跑起来"，
//! 它当前是不是在跑属于实现细节。所以 exited/created 都不报错，直接起。
//!
//! # 前提：容器得记着自己是怎么起来的
//!
//! `create` 和 `run -d` 都会落一份启动配置（`runstate::save_run_args`）。
//! 前台 `run` 的容器退出即连目录一起清掉，本就不存在"重启"这回事。

use crate::error::Result;
use crate::runstate::{self, Liveness};

struct Options<'a> {
    names: Vec<&'a str>,
    timeout: Option<&'a str>,
}

fn parse<'a>(args: &'a [String]) -> Result<Options<'a>> {
    let mut names = Vec::new();
    let mut timeout = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-t" | "--timeout" => {
                i += 1;
                timeout = Some(
                    args.get(i)
                        .map(|s| s.as_str())
                        .ok_or_else(|| {
                            crate::error::WboxError::args("restart: --timeout 缺少取值（秒）")
                        })?,
                );
            }
            other if other.starts_with('-') => {
                return Err(crate::error::WboxError::args(format!(
                    "restart: 未知参数 '{}'（用法：wbox restart [--timeout <秒>] <NAME>...）",
                    other
                )))
            }
            other => names.push(other),
        }
        i += 1;
    }
    if names.is_empty() {
        return Err(crate::error::WboxError::args(
            "restart: 缺少容器名（用法：wbox restart [--timeout <秒>] <NAME>...）",
        ));
    }
    Ok(Options { names, timeout })
}

pub fn cmd_restart(args: &[String]) -> Result<u32> {
    let o = parse(args)?;
    // 多个名字时不一遇错就中断（与 `rm` 同一取舍）：后面的名字可能是好的，
    // 中断会让用户以为全都没重启。逐个执行、逐个报告，最后用退出码汇总。
    let mut failed = 0usize;
    for name in &o.names {
        if let Err(e) = restart_one(name, o.timeout) {
            eprintln!("wbox: 重启 '{}' 失败：{}", name, e);
            failed += 1;
            continue;
        }
        println!("{}", name);
    }
    if failed > 0 {
        return Err(crate::error::WboxError::args(format!(
            "{} 个容器未能重启（共 {} 个）",
            failed,
            o.names.len()
        )));
    }
    Ok(0)
}

fn restart_one(name: &str, timeout: Option<&str>) -> Result<()> {
    let dir = runstate::resolve_existing(name)?;
    if runstate::liveness(&dir) == Liveness::Running {
        let secs = match timeout {
            Some(t) => t.parse::<u64>().map_err(|_| {
                crate::error::WboxError::args(format!("restart: --timeout 取值 '{}' 不是合法秒数", t))
            })?,
            None => super::stop::DEFAULT_TIMEOUT_SECS,
        };
        // 走 stop 自己那条路（先礼后兵、Windows 的 Job 处理都在里面），
        // 只是让它别打名字——restart 会在整轮结束时按批次汇报。
        super::stop::stop_quiet(name, secs)?;
    }
    // stop 之后走与 `wbox start` 完全相同的一条路：在操作锁内原子领取配置，
    // 再拉起 detached supervisor。
    let (run_args, reservation) = runstate::activate_created(name)?;
    super::run::spawn_reserved_quiet(name.to_string(), &run_args, reservation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn parse_takes_names_and_timeout() {
        let a: Vec<String> = ["-t", "3", "one", "two"].iter().map(|s| s.to_string()).collect();
        let o = parse(&a).unwrap();
        assert_eq!(o.names, vec!["one", "two"]);
        assert_eq!(o.timeout, Some("3"));
        assert!(parse(&[]).is_err(), "缺容器名应报错");
        assert!(parse(&["--bogus".to_string()]).is_err());
        assert!(parse(&["-t".to_string()]).is_err(), "缺取值应报错");
    }

    #[test]
    fn unknown_container_is_an_error() {
        let _home = TempHome::new("restartunknown");
        assert!(cmd_restart(&["nope".to_string()]).is_err());
    }

    /// 没有启动配置的容器要说清**哪两种容器**记得住配置，而不是只说"没有配置"。
    #[test]
    fn container_without_saved_config_explains_which_kinds_keep_one() {
        let _home = TempHome::new("restartnocfg");
        let dir = runstate::dir_for("c").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let m = format!("{}", restart_one("c", None).unwrap_err());
        assert!(m.contains("run -d"), "要说清哪种容器留得住配置：{}", m);
    }
}
