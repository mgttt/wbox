//! `wbox start`：原子领取 create 保存的配置并启动 detached supervisor。

use crate::error::Result;

pub fn cmd_start(args: &[String]) -> Result<u32> {
    let names = super::args::take_container_names(args, "start")?;
    for name in names {
        let (run_args, reservation) = crate::runstate::activate_created(name)?;
        super::run::spawn_reserved(name.to_string(), &run_args, reservation)?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 名字解析用的是共享原语（`args::take_container_names`），这里只确认
    /// start 确实接了上去——解析规则本身由那边的单测覆盖。
    #[test]
    fn rejects_flags_and_missing_names() {
        assert!(cmd_start(&[]).is_err());
        assert!(cmd_start(&["-a".to_string(), "one".to_string()]).is_err());
    }
}
