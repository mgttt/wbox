//! `wbox rm`：删除容器记录（`PRD.md` F8.3、F9.29）。
//!
//! 默认语义刻意收窄：**只删记录，不碰进程**。运行中的容器一律拒绝——不带 `-f`
//! 的 `rm` 停不掉容器，真删了只会让它从 `ps` 里消失、变成没人管得到的孤儿。
//!
//! `-f/--force` 才是"先停再删"（F9.29，与 docker 一致）。这个能力必须**显式
//! 要求**：删记录和杀进程是两件危险程度差很多的事，默认把后者也做了，
//! 一次手误就是把还在干活的容器打断。停的那一步直接复用 `stop` 那条路
//! （先礼后兵、Windows 的 Job 处理都在里面），不另写一份。

use crate::error::Result;
use crate::runstate;

pub fn cmd_rm(args: &[String]) -> Result<u32> {
    let mut force = false;
    let mut rest: Vec<String> = Vec::new();
    for a in args {
        match a.as_str() {
            "-f" | "--force" => force = true,
            other => rest.push(other.to_string()),
        }
    }
    let names = super::args::take_container_names(&rest, "rm")?;
    let owned: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    // "一个失败不中断后面的"这条取舍在 rm/prune/restart/compose down 各有一份，
    // 已收敛到 args::each_named（含逐条报错与末尾汇总）。
    super::args::each_named(&owned, "删除", super::args::Echo::Name, |name| {
        if force {
            // 已经停了的容器 stop 是幂等的，所以不必先判活再决定要不要停；
            // 少一次判活也就少一个"判完到停之间容器状态变了"的窗口。
            super::stop::stop_quiet(name, super::stop::DEFAULT_TIMEOUT_SECS)?;
        }
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
        // 单个容器时错误要直接说明原因，而不是"1 个容器未成功"这种汇总
        assert!(format!("{}", err).contains("仍在运行"), "{}", err);
        // 真正要钉的是**记录还在**：措辞可以变，"运行中的容器没被删掉"不能变
        assert!(
            runstate::dir_for("live").unwrap().exists(),
            "运行中的容器记录不该被删掉"
        );
        drop(reg);
    }

    /// 不带 `-f` 时拒绝运行中的容器——那是"rm 不会替你停容器"这条约定的全部意义。
    /// `-f` 会被认出来且不影响正常删除。
    ///
    /// **`-f` 真的把运行中的容器停掉**这一条不在这里验：那需要一个真的
    /// supervisor 进程，单测里的 `Registration` 一 drop 记录就没了，
    /// 摆不出"还在跑"的现场。由门禁 RMF.1 在真容器上取证——与其在这里
    /// 摆一个假的现场自欺，不如把它交给能造出真现场的那一层。
    #[test]
    fn force_flag_is_accepted_and_does_not_break_normal_removal() {
        let _home = TempHome::new("rmforce");
        let reg = runstate::register("live2", &["/bin/true".into()], "(native)").unwrap();
        assert!(cmd_rm(&["live2".to_string()]).is_err(), "不带 -f 要拒绝");
        assert!(runstate::dir_for("live2").unwrap().exists());
        drop(reg);

        let dir = plant_stale("stale");
        assert_eq!(cmd_rm(&["-f".to_string(), "stale".to_string()]).unwrap(), 0);
        assert!(!dir.exists());
        // 选项可以写在名字后面
        let dir = plant_stale("stale2");
        assert_eq!(cmd_rm(&["stale2".to_string(), "--force".to_string()]).unwrap(), 0);
        assert!(!dir.exists());
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
