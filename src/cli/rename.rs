//! `wbox rename`：给容器改名（`PRD.md` F9.27）。
//!
//! 命令本身只做参数解析，真正的工作在 [`crate::runstate::rename`]——它要在
//! 根操作锁内完成，与 `rm`/`run` 的并发安全是同一套机制。

use crate::error::{Result, WboxError};

fn parse(args: &[String]) -> Result<(&str, &str)> {
    let mut pos = Vec::new();
    for a in args {
        if a.starts_with('-') {
            return Err(WboxError::args(format!(
                "rename: 未知参数 '{}'（用法：wbox rename <旧名> <新名>）",
                a
            )));
        }
        pos.push(a.as_str());
    }
    if pos.len() != 2 {
        return Err(WboxError::args(
            "rename 需要两个参数：wbox rename <旧名> <新名>",
        ));
    }
    Ok((pos[0], pos[1]))
}

pub fn cmd_rename(args: &[String]) -> Result<u32> {
    let (old, new) = parse(args)?;
    crate::runstate::rename(old, new)?;
    println!("{}", new);
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runstate;
    use crate::testenv::TempHome;

    #[test]
    fn parse_requires_exactly_two_names() {
        assert_eq!(
            parse(&["a".to_string(), "b".to_string()]).unwrap(),
            ("a", "b")
        );
        assert!(parse(&[]).is_err());
        assert!(parse(&["a".to_string()]).is_err());
        assert!(parse(&["a".to_string(), "b".to_string(), "c".to_string()]).is_err());
        assert!(parse(&["-f".to_string(), "a".to_string()]).is_err());
    }

    /// 改名要连 `meta.json` 里的名字一起改：只改目录名的话，`ps` 还会显示旧名
    /// ——目录名和记录名各说各话，比不支持改名更让人困惑。
    #[test]
    fn renames_directory_and_the_recorded_name() {
        let _home = TempHome::new("renameok");
        let dir = runstate::dir_for("old").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("meta.json"),
            r#"{"name":"old","pid":1,"created_unix":1,"cmd":["x"],"target":"t"}"#,
        )
        .unwrap();

        assert_eq!(
            cmd_rename(&["old".to_string(), "new".to_string()]).unwrap(),
            0
        );
        assert!(!dir.exists(), "旧目录应已不在");
        let newdir = runstate::dir_for("new").unwrap();
        assert!(newdir.exists());
        assert_eq!(
            runstate::read_meta(&newdir).unwrap().name,
            "new",
            "meta.json 里的名字也要跟着改"
        );
    }

    #[test]
    fn rejects_unknown_same_name_and_occupied_target() {
        let _home = TempHome::new("renamebad");
        assert!(cmd_rename(&["nope".to_string(), "x".to_string()]).is_err());

        for n in ["a", "b"] {
            std::fs::create_dir_all(runstate::dir_for(n).unwrap()).unwrap();
        }
        // 目标名已被占用：静默覆盖会把另一个容器的记录整个抹掉
        let m = format!(
            "{}",
            cmd_rename(&["a".to_string(), "b".to_string()]).unwrap_err()
        );
        assert!(m.contains("已被占用"), "{}", m);
        assert!(
            runstate::dir_for("a").unwrap().exists(),
            "被拒绝时不该已经动过旧目录"
        );
        // 同名改名是无操作，明说而不是假装成功
        assert!(cmd_rename(&["a".to_string(), "a".to_string()]).is_err());
    }
}
