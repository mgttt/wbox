//! `wbox commit`：把容器的改动固化成新镜像（`PRD.md` F9.20）。
//!
//! 这一层只做参数解析；实现放在 `build.rs`，因为它复用的四套机制
//! （硬链接铺基础层、overlay 合并、增量层、分层 manifest）都在那里。
//! 跨文件调用只会把 `pub(crate)` 的口子越开越大。

use crate::error::{Result, WboxError};

struct Options<'a> {
    container: &'a str,
    tag: &'a str,
}

fn parse<'a>(args: &'a [String]) -> Result<Options<'a>> {
    let mut pos: Vec<&str> = Vec::new();
    for a in args {
        if a.starts_with('-') {
            return Err(WboxError::args(format!(
                "commit: 未知参数 '{}'（用法：wbox commit <容器> <镜像[:标签]>）",
                a
            )));
        }
        pos.push(a);
    }
    match pos.len() {
        2 => Ok(Options {
            container: pos[0],
            tag: pos[1],
        }),
        // 不支持"省略镜像名自动生成"：docker 会给个随机 ID，而那对
        // wbox 的按名寻址缓存布局没有意义，还会留下一堆没人认领的镜像。
        _ => Err(WboxError::args("commit 需要两个参数：<容器> <镜像[:标签]>")),
    }
}

pub fn cmd_commit(args: &[String]) -> Result<u32> {
    let o = parse(args)?;
    #[cfg(not(windows))]
    {
        crate::build::commit_container(o.container, o.tag)
    }
    #[cfg(windows)]
    {
        // 把参数带进错误信息：用户看到自己写的容器名/标签，才好确认命令没敲错
        // ——顺带也就不存在"解析了却没人读"的字段。
        Err(WboxError::args(format!(
            "commit（{} → {}）目前只在 Linux 宿主可用：它靠 overlay 可写层取得容器的改动，\
             Windows 侧尚未提供该层（PRD F9.12/F9.20）",
            o.container, o.tag
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn parse_requires_two_positionals() {
        let a: Vec<String> = ["c", "img:v1"].iter().map(|s| s.to_string()).collect();
        let o = parse(&a).unwrap();
        assert_eq!(o.container, "c");
        assert_eq!(o.tag, "img:v1");
        assert!(parse(&[]).is_err());
        assert!(parse(&["only".to_string()]).is_err());
        assert!(parse(&["a".to_string(), "b".to_string(), "c".to_string()]).is_err());
        assert!(parse(&["-x".to_string(), "y".to_string()]).is_err());
    }

    #[test]
    fn unknown_container_is_an_error() {
        let _home = TempHome::new("commitunknown");
        assert!(cmd_commit(&["nope".to_string(), "img:v1".to_string()]).is_err());
    }

    /// 没有 overlay 层就拿不到"改了什么"。静默 commit 一份与镜像完全相同的
    /// 副本更糟——用户会以为改动固化了。
    #[cfg(not(windows))]
    #[test]
    fn missing_overlay_layer_explains_instead_of_committing_a_copy() {
        let _home = TempHome::new("commitnolayer");
        let dir = crate::runstate::dir_for("c").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let m = format!(
            "{}",
            cmd_commit(&["c".to_string(), "img:v1".to_string()]).unwrap_err()
        );
        assert!(m.contains("overlay"), "{}", m);
    }
}
