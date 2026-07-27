//! `--restart` 重启策略（`PRD.md` F9.6）。
//!
//! 语义与 docker 对齐：`no`（默认）、`on-failure[:次数]`、`always`。
//!
//! # 谁来重启
//!
//! 由 **supervisor 自己**在 guest 退出后重跑，而不是另起一个守护进程。
//! 这带来一个恰到好处的性质：`wbox stop` 终止的正是 supervisor，
//! **停掉的容器不会被自己重新拉起**——不需要额外的"是不是人为停的"标记，
//! 语义天然正确。
//!
//! 相对地，supervisor 崩溃时重启也就随之失效。这是刻意的取舍：要覆盖那种
//! 情况就得引入常驻守护进程，与"免安装、无服务"（PRD §2.2）冲突。

use crate::error::{Result, WboxError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RestartPolicy {
    /// 不重启（默认）
    #[default]
    No,
    /// 非零退出码才重启；`Some(n)` 为最多重试次数
    OnFailure(Option<u32>),
    /// 无论退出码都重启
    Always,
}

impl RestartPolicy {
    /// 给定这次的退出码与**已经重启过的次数**，判断是否还要再来一次。
    pub fn should_restart(&self, exit_code: u32, already: u32) -> bool {
        match self {
            Self::No => false,
            Self::Always => true,
            // 退出码 0 视为"活儿干完了"，不属于失败
            Self::OnFailure(None) => exit_code != 0,
            Self::OnFailure(Some(max)) => exit_code != 0 && already < *max,
        }
    }
}

/// 解析 `--restart` 取值。
pub fn parse_restart(spec: &str) -> Result<RestartPolicy> {
    let bad = || {
        WboxError::args(format!(
            "--restart '{}' 无效（支持 no / on-failure[:次数] / always）",
            spec
        ))
    };
    match spec {
        "no" => Ok(RestartPolicy::No),
        "always" => Ok(RestartPolicy::Always),
        "on-failure" => Ok(RestartPolicy::OnFailure(None)),
        other => {
            let rest = other.strip_prefix("on-failure:").ok_or_else(bad)?;
            let n: u32 = rest.parse().map_err(|_| bad())?;
            Ok(RestartPolicy::OnFailure(Some(n)))
        }
    }
}

/// `--restart` 与 `--rm` 冲突时报错。
///
/// 与 docker 同样的理由：`--rm` 是"退出即删记录"，`--restart` 是"退出后再拉
/// 起来"，两者对"退出"的处置直接矛盾。静默让其中一个赢，用户无从知道实际
/// 生效的是哪个。
pub fn reject_conflicting_rm(policy: RestartPolicy, auto_remove: bool) -> Result<()> {
    if policy == RestartPolicy::No || !auto_remove {
        return Ok(());
    }
    Err(WboxError::args(
        "--restart 与 --rm 不能同时使用：前者要在退出后重新拉起，后者要退出即\
         删记录，对'退出'的处置互相矛盾",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_forms() {
        assert_eq!(parse_restart("no").unwrap(), RestartPolicy::No);
        assert_eq!(parse_restart("always").unwrap(), RestartPolicy::Always);
        assert_eq!(
            parse_restart("on-failure").unwrap(),
            RestartPolicy::OnFailure(None)
        );
        assert_eq!(
            parse_restart("on-failure:3").unwrap(),
            RestartPolicy::OnFailure(Some(3))
        );
        for bad in ["", "yes", "on-failure:", "on-failure:x", "always:2"] {
            assert!(parse_restart(bad).is_err(), "'{}' 应被拒绝", bad);
        }
    }

    /// 退出码 0 是"活儿干完了"，`on-failure` 不该把它当失败重启。
    #[test]
    fn on_failure_ignores_success() {
        let p = RestartPolicy::OnFailure(None);
        assert!(!p.should_restart(0, 0));
        assert!(p.should_restart(1, 0));
        assert!(p.should_restart(1, 100), "无上限时不该自己停下");
    }

    /// 有上限时必须真的停下——否则一个必然失败的容器会无限重启刷爆日志。
    #[test]
    fn on_failure_respects_max() {
        let p = RestartPolicy::OnFailure(Some(2));
        assert!(p.should_restart(1, 0));
        assert!(p.should_restart(1, 1));
        assert!(!p.should_restart(1, 2), "达到上限必须停");
    }

    #[test]
    fn always_and_no_are_unconditional() {
        assert!(RestartPolicy::Always.should_restart(0, 999));
        assert!(!RestartPolicy::No.should_restart(1, 0));
    }

    #[test]
    fn rm_conflict_is_explicit() {
        assert!(reject_conflicting_rm(RestartPolicy::No, true).is_ok());
        assert!(reject_conflicting_rm(RestartPolicy::Always, false).is_ok());
        let e = reject_conflicting_rm(RestartPolicy::Always, true).unwrap_err();
        assert!(format!("{}", e).contains("不能同时使用"), "{}", e);
    }
}
