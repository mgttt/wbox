//! `wbox create`：验证并保存运行配置，但不启动 workload。

use crate::error::Result;

pub fn cmd_create(args: &[String]) -> Result<u32> {
    let summary = super::run::prepare_create(args)?;
    crate::runstate::create_pending(
        &summary.name,
        &summary.args,
        &summary.cmd,
        &summary.target,
        Some(summary.exec_context),
    )?;
    println!("{}", summary.name);
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runstate::{self, Liveness};
    use crate::testenv::TempHome;

    #[test]
    fn creates_native_record_without_running_it() {
        let _home = TempHome::new("create-native");
        let args = vec![
            "--name".to_string(),
            "made".to_string(),
            "--".to_string(),
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ];
        assert_eq!(cmd_create(&args).unwrap(), 0);
        let dir = runstate::dir_for("made").unwrap();
        assert_eq!(runstate::liveness(&dir), Liveness::Created);
        assert_eq!(runstate::read_meta(&dir).unwrap().pid, 0);
        assert!(!dir.join("lock").exists());
    }

    #[test]
    fn rejects_detach_and_duplicate_name() {
        let _home = TempHome::new("create-reject");
        assert!(cmd_create(&[
            "-d".to_string(),
            "--".to_string(),
            "x".to_string()
        ])
        .is_err());
        let args = vec![
            "--name".to_string(),
            "same".to_string(),
            "--".to_string(),
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ];
        cmd_create(&args).unwrap();
        assert!(cmd_create(&args).is_err());
    }
}
