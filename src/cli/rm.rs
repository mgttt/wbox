//! `wbox rm`：删除已退出的容器记录（`PRD.md` F8.3）。
//!
//! 语义刻意收窄：**只删记录，不碰进程**。运行中的容器一律拒绝——`rm` 停不掉
//! 容器，真删了只会让它从 `ps` 里消失、变成没人管得到的孤儿。停容器是 F8.3
//! `stop` 的事。

use crate::error::Result;
use crate::runstate;

pub fn cmd_rm(args: &[String]) -> Result<u32> {
    let names = super::args::take_container_names(args, "rm")?;
    let owned: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    // "一个失败不中断后面的"这条取舍在 rm/prune/restart/compose down 各有一份，
    // 已收敛到 args::each_named（含逐条报错与末尾汇总）。
    super::args::each_named(&owned, "删除", super::args::Echo::Name, |name| {
        runstate::remove(name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;
    use std::path::PathBuf;

    /// 造一条"已退出"的残留记录（有锁文件但无人持有）。
    fn plant_stale(name: &str) -> PathBuf {
        let dir = runstate::dir_for(name).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lock"), b"").unwrap();
        std::fs::write(
            dir.join("meta.json"),
            format!(
                r#"{{"name":"{}","pid":1,"created_unix":1,"cmd":["x"],"target":"t"}}"#,
                name
            ),
        )
        .unwrap();
        dir
    }

    #[test]
    fn removes_exited_record() {
        let _home = TempHome::new("exited");
        let dir = plant_stale("gone");
        assert_eq!(cmd_rm(&["gone".to_string()]).unwrap(), 0);
        assert!(!dir.exists(), "状态目录应已删除");
    }

    /// 运行中的必须拒绝——rm 不会停容器，删了记录就等于放跑一个孤儿。
    #[test]
    fn refuses_running_container() {
        let _home = TempHome::new("running");
        let reg = runstate::register("live", &["/bin/true".into()], "(native)").unwrap();
        let err = cmd_rm(&["live".to_string()]).unwrap_err();
        assert!(format!("{}", err).contains("失败"), "{}", err);
        // 真正要钉的是**记录还在**：措辞可以变，"运行中的容器没被删掉"不能变
        assert!(
            runstate::dir_for("live").unwrap().exists(),
            "运行中的容器记录不该被删掉"
        );
        drop(reg);
    }

    #[test]
    fn unknown_name_is_an_error() {
        let _home = TempHome::new("unknown");
        assert!(cmd_rm(&["nope".to_string()]).is_err());
    }

    /// 一个名字失败不该中断其余的：好的要真删掉，退出码仍反映有失败。
    #[test]
    fn continues_past_failures_and_still_reports() {
        let _home = TempHome::new("mixed");
        let ok_dir = plant_stale("good");
        let err = cmd_rm(&["missing".to_string(), "good".to_string()]).unwrap_err();
        assert!(!ok_dir.exists(), "失败的名字不该挡住后面能删的");
        assert!(format!("{}", err).contains("1 个"), "{}", err);
    }

    #[test]
    fn rejects_flags_and_empty_args() {
        let _home = TempHome::new("args");
        assert!(cmd_rm(&[]).is_err());
        assert!(cmd_rm(&["--force".to_string()]).is_err());
    }

    /// 容器名不得逃出状态根目录（与 register 同一道防线）。
    #[test]
    fn name_cannot_escape_run_root() {
        let _home = TempHome::new("escape");
        assert!(cmd_rm(&["../evil".to_string()]).is_err());
    }
}
