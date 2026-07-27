//! `wbox start`：原子领取 create 保存的配置并启动 detached supervisor。

use crate::error::{Result, WboxError};

fn parse(args: &[String]) -> Result<Vec<&str>> {
    if args.is_empty() {
        return Err(WboxError::args(
            "start: 缺少容器名（用法：wbox start <NAME>...）",
        ));
    }
    let mut names = Vec::new();
    for arg in args {
        if arg.starts_with('-') {
            return Err(WboxError::args(format!(
                "start: 当前不支持参数 '{}'（用法：wbox start <NAME>...）",
                arg
            )));
        }
        names.push(arg.as_str());
    }
    Ok(names)
}

pub fn cmd_start(args: &[String]) -> Result<u32> {
    let names = parse(args)?;
    for name in names {
        let (run_args, reservation) = crate::runstate::activate_created(name)?;
        super::run::spawn_reserved(name.to_string(), &run_args, reservation)?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_names_and_rejects_flags() {
        assert_eq!(
            parse(&["one".to_string(), "two".to_string()]).unwrap(),
            vec!["one", "two"]
        );
        assert!(parse(&[]).is_err());
        assert!(parse(&["-a".to_string(), "one".to_string()]).is_err());
    }
}
