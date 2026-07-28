//! `wbox volume`：命名卷的 create / ls / rm / inspect（`PRD.md` F9.35）。
//!
//! 与 `image` / `container` 一样是**分组动词**：自己带子命令。
//! 真正的逻辑在 [`crate::volume`]，这里只做参数解析与展示。

use crate::error::{Result, WboxError};
use crate::volume;

pub fn cmd_volume(args: &[String]) -> Result<u32> {
    match args.first().map(String::as_str) {
        Some("create") => create(&args[1..]),
        Some("ls") | Some("list") => list(&args[1..]),
        Some("rm") | Some("remove") => remove(&args[1..]),
        Some("inspect") => inspect(&args[1..]),
        Some(other) => Err(WboxError::args(format!(
            "未知 volume 子命令 '{}'（支持 create / ls / rm / inspect）",
            other
        ))),
        None => Err(WboxError::args(
            "volume 缺少子命令（支持 create / ls / rm / inspect）",
        )),
    }
}

fn create(args: &[String]) -> Result<u32> {
    let names = super::args::take_container_names(args, "volume create")?;
    let owned: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    super::args::each_named(&owned, "创建卷", super::args::Echo::Name, |n| {
        volume::create(n).map(|_| ())
    })
}

fn list(args: &[String]) -> Result<u32> {
    let mut quiet = false;
    for a in args {
        match a.as_str() {
            "-q" | "--quiet" => quiet = true,
            other => {
                return Err(WboxError::args(format!(
                    "volume ls 不支持参数 '{}'（支持 -q/--quiet）",
                    other
                )))
            }
        }
    }
    let names = volume::list()?;
    if quiet {
        // 与 `ps -q` / `images -q` 同一意图：只出名字，供命令替换直接使用
        for n in &names {
            println!("{}", n);
        }
        return Ok(0);
    }
    if names.is_empty() {
        println!("没有已创建的卷。");
        return Ok(0);
    }
    println!("{:<24} {:<10} 使用中的容器", "卷名", "占用");
    for n in &names {
        let users = volume::users(n);
        let size = dir_size(&volume::dir_for(n)?);
        println!(
            "{:<24} {:<10} {}",
            n,
            size,
            if users.is_empty() {
                "-".to_string()
            } else {
                users.join(", ")
            }
        );
    }
    Ok(0)
}

/// 卷占用的磁盘，人读形式。取不到（权限等）时给 `-`，不猜也不报错——
/// 这一列是参考信息，不该让整条 `ls` 失败。
fn dir_size(dir: &std::path::Path) -> String {
    fn walk(p: &std::path::Path) -> Option<u64> {
        let meta = std::fs::symlink_metadata(p).ok()?;
        if meta.is_dir() {
            let mut sum = 0u64;
            for e in std::fs::read_dir(p).ok()?.flatten() {
                sum += walk(&e.path()).unwrap_or(0);
            }
            return Some(sum);
        }
        Some(meta.len())
    }
    match walk(dir) {
        Some(n) => super::stats::human_bytes(n),
        None => "-".to_string(),
    }
}

fn remove(args: &[String]) -> Result<u32> {
    let mut force = false;
    let mut rest: Vec<String> = Vec::new();
    for a in args {
        match a.as_str() {
            "-f" | "--force" => force = true,
            other => rest.push(other.to_string()),
        }
    }
    let names = super::args::take_container_names(&rest, "volume rm")?;
    let owned: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    super::args::each_named(&owned, "删除卷", super::args::Echo::Name, |n| {
        volume::remove(n, force).map(|_| ())
    })
}

fn inspect(args: &[String]) -> Result<u32> {
    let names = super::args::take_container_names(args, "volume inspect")?;
    let mut out = Vec::new();
    for n in &names {
        let dir = volume::dir_for(n)?;
        if !dir.is_dir() {
            return Err(WboxError::args(format!("卷 '{}' 不存在", n)));
        }
        out.push(serde_json::json!({
            "Name": n,
            // 与 docker 的字段名对齐；wbox 只有 local 一种驱动（没有插件机制）
            "Driver": "local",
            "Mountpoint": dir.to_string_lossy(),
            "UsedBy": volume::users(n),
        }));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::Value::Array(out))
            .map_err(|e| WboxError::args(format!("序列化失败：{}", e)))?
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn unknown_and_missing_subcommand_list_what_is_supported() {
        let m = format!("{}", cmd_volume(&[]).unwrap_err());
        assert!(m.contains("create / ls / rm / inspect"), "{}", m);
        let m = format!("{}", cmd_volume(&["bogus".to_string()]).unwrap_err());
        assert!(m.contains("create / ls / rm / inspect"), "{}", m);
    }

    #[test]
    fn create_list_inspect_remove_round_trip() {
        let _home = TempHome::new("volcli");
        assert_eq!(cmd_volume(&["create".to_string(), "v1".to_string()]).unwrap(), 0);
        assert_eq!(volume::list().unwrap(), vec!["v1"]);
        assert_eq!(cmd_volume(&["ls".to_string()]).unwrap(), 0);
        assert_eq!(cmd_volume(&["ls".to_string(), "-q".to_string()]).unwrap(), 0);
        assert_eq!(cmd_volume(&["inspect".to_string(), "v1".to_string()]).unwrap(), 0);
        assert_eq!(cmd_volume(&["rm".to_string(), "v1".to_string()]).unwrap(), 0);
        assert!(volume::list().unwrap().is_empty());
        // 删不存在的要报错，而不是静默成功——静默会让脚本以为清理过了
        assert!(cmd_volume(&["rm".to_string(), "v1".to_string()]).is_err());
        assert!(cmd_volume(&["inspect".to_string(), "v1".to_string()]).is_err());
    }

    #[test]
    fn ls_rejects_unknown_flags() {
        let _home = TempHome::new("volls");
        assert!(cmd_volume(&["ls".to_string(), "-a".to_string()]).is_err());
    }
}
