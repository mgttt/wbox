//! `wbox wait`：等待一个或多个容器退出并打印 guest 退出码。

use crate::error::{Result, WboxError};
use crate::runstate::{self, Liveness};
use std::time::Duration;

fn wait_one(name: &str) -> Result<u32> {
    let dir = runstate::resolve_existing(name)?;
    loop {
        if !dir.exists() {
            return Err(WboxError::args(format!(
                "容器 '{}' 在 wait 期间被删除",
                name
            )));
        }
        if runstate::liveness(&dir) == Liveness::Exited {
            return runstate::read_exit_code(&dir).ok_or_else(|| {
                WboxError::args(format!(
                    "容器 '{}' 已退出，但该记录没有退出码（可能来自旧版或异常崩溃）",
                    name
                ))
            });
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub fn cmd_wait(args: &[String]) -> Result<u32> {
    if args.is_empty() {
        return Err(WboxError::args("wait 缺少容器名"));
    }
    for name in args {
        if name.starts_with('-') {
            return Err(WboxError::args(format!("wait: 未知参数 '{}'", name)));
        }
        println!("{}", wait_one(name)?);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn wait_requires_names_and_rejects_flags() {
        assert!(cmd_wait(&[]).is_err());
        assert!(cmd_wait(&["--bogus".to_string()]).is_err());
    }

    #[test]
    fn wait_reads_persisted_exit_code() {
        let _home = TempHome::new("wait-exit");
        let mut reservation = runstate::reserve_detached("done").unwrap();
        let token = reservation.token().to_string();
        let reg = runstate::register_with_context(
            "done",
            &["false".into()],
            "(native)",
            true,
            None,
            Some(&token),
        )
        .unwrap();
        reservation.disarm();
        runstate::record_exit_code(reg.dir(), 17).unwrap();
        drop(reg);

        assert_eq!(wait_one("done").unwrap(), 17);
        runstate::remove("done").unwrap();
    }

    #[test]
    fn wait_explains_missing_exit_code() {
        let _home = TempHome::new("wait-unknown");
        let mut reservation = runstate::reserve_detached("crashed").unwrap();
        let token = reservation.token().to_string();
        let reg = runstate::register_with_context(
            "crashed",
            &["x".into()],
            "(native)",
            true,
            None,
            Some(&token),
        )
        .unwrap();
        reservation.disarm();
        drop(reg);

        let err = wait_one("crashed").unwrap_err();
        assert!(format!("{}", err).contains("没有退出码"), "{}", err);
        runstate::remove("crashed").unwrap();
    }
}
