//! 命名卷（`PRD.md` F9.35）：由 wbox 管理生命周期的持久目录。
//!
//! # 它和绑定挂载解决的不是同一个问题
//!
//! `-v /宿主/某处:/data` 要求调用方**自己想好数据放哪**，路径写错就挂错。
//! 命名卷把「放哪」这件事交给 wbox：`-v mydata:/data` 里的 `mydata` 是一个
//! 名字，实际落在 `~/.wbox/volumes/mydata`。容器删了卷还在，换个容器挂同一个
//! 名字就接着用——这正是 docker/podman 里命名卷存在的理由，也是「有状态服务」
//! 这类场景绕不开的东西。
//!
//! # 为什么 rootless 也能做
//!
//! 卷就是用户自己 `$HOME` 下的一个目录，绑定挂载复用 F9.1 已有的那条路径。
//! 不需要驱动、不需要常驻服务、不需要 root——与 §2.2 的前提不冲突。
//!
//! # 名字与路径怎么区分
//!
//! 与 docker 同一条规则：**源里不含 `/` 就是卷名，含 `/` 就是宿主路径**。
//! 这条规则必须简单且可预测，因为它决定了「数据写到哪」；任何「先试试当路径、
//! 不存在就当名字」的聪明做法都会让一次手滑变成一个新建的空卷。

use crate::error::{Result, WboxError};
use std::path::PathBuf;

/// 卷根目录：`~/.wbox/volumes`。与镜像缓存、运行状态同在 `~/.wbox` 下，
/// 便于统一清理。
pub fn volumes_root() -> Result<PathBuf> {
    let root = crate::paths::root()
        .map_err(|error| WboxError::args(format!("无法确定用户主目录：{error}")))?;
    Ok(root.join("volumes"))
}

/// 这个源串是不是**卷名**（而不是宿主路径）。
///
/// 判据与 docker 一致：不含路径分隔符即为名字。`.`/`..` 单独列出来挡掉——
/// 它们不含 `/` 却明显是路径意图，当成卷名会造出一个叫 `..` 的目录。
pub fn looks_like_name(src: &str) -> bool {
    !src.is_empty() && !src.contains('/') && !src.contains('\\') && src != "." && src != ".."
}

/// 校验卷名并给出它的目录。
///
/// 名字要落成目录名，所以必须挡住路径穿越。这里不做「净化」（把非法字符换掉）
/// 而是**直接拒绝**：净化会让两个不同的名字映射到同一个目录，用户以为挂的是两个
/// 卷，其实是一个。
pub fn dir_for(name: &str) -> Result<PathBuf> {
    if !looks_like_name(name) {
        return Err(WboxError::args(format!(
            "卷名 '{}' 非法：不能为空、不能含路径分隔符，也不能是 '.' 或 '..'",
            name
        )));
    }
    if name.starts_with('-') {
        return Err(WboxError::args(format!(
            "卷名 '{}' 非法：不能以 '-' 开头（会被当成选项）",
            name
        )));
    }
    Ok(volumes_root()?.join(name))
}

/// 建卷（已存在时是无操作，不报错——与 docker 一致，好让脚本反复跑）。
/// 返回值表示这次是不是**新建**的，调用方据此决定要不要出声。
pub fn create(name: &str) -> Result<(PathBuf, bool)> {
    let dir = dir_for(name)?;
    if dir.is_dir() {
        return Ok((dir, false));
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| WboxError::args(format!("创建卷 '{}' 失败：{}", name, e)))?;
    Ok((dir, true))
}

/// 列出已有的卷（按名字排序）。
pub fn list() -> Result<Vec<String>> {
    let root = volumes_root()?;
    let Ok(rd) = std::fs::read_dir(&root) else {
        // 目录不存在 = 还没建过卷，不是错误
        return Ok(Vec::new());
    };
    let mut out: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .collect();
    out.sort();
    Ok(out)
}

/// 哪些**运行中**的容器正在用这个卷。
///
/// 数据来自 `ExecContext.volumes`（F9.33 为了让 `inspect` 如实报出挂载而记的），
/// 这里直接复用——不另存一份「谁在用哪个卷」的引用计数：那份账会在容器崩溃时
/// 过期，而从容器记录现算永远是当前的。
pub fn users(name: &str) -> Vec<String> {
    let Ok(dir) = dir_for(name) else {
        return Vec::new();
    };
    let Ok(all) = crate::runstate::list() else {
        return Vec::new();
    };
    all.into_iter()
        .filter(|(_, live)| *live == crate::runstate::Liveness::Running)
        .filter(|(e, _)| {
            e.exec_context.as_ref().is_some_and(|c| {
                c.volumes.iter().any(|v| {
                    // 从右侧切 guest/mode，Windows 盘符里的冒号属于 host。
                    crate::backend::recorded_volume_parts(v)
                        .is_some_and(|(host, _, _)| std::path::Path::new(host) == dir)
                })
            })
        })
        .map(|(e, _)| e.name)
        .collect()
}

/// 删卷。`force` 为假且**有运行中的容器在用**时拒绝。
///
/// 拒绝的理由不是洁癖：卷目录被抽掉时，容器里那个挂载点会变成一个悬空的挂载，
/// 之后往里写的数据谁也找不回来。让用户先停容器，比事后解释数据去哪了强。
pub fn remove(name: &str, force: bool) -> Result<PathBuf> {
    let dir = dir_for(name)?;
    if !dir.is_dir() {
        return Err(WboxError::args(format!("卷 '{}' 不存在", name)));
    }
    if !force {
        let busy = users(name);
        if !busy.is_empty() {
            return Err(WboxError::args(format!(
                "卷 '{}' 正被运行中的容器使用（{}）：先停掉它们，或用 -f 强删\
                 （强删会让那些容器里的挂载点悬空，之后写进去的数据找不回来）",
                name,
                busy.join(", ")
            )));
        }
    }
    std::fs::remove_dir_all(&dir)
        .map_err(|e| WboxError::args(format!("删除卷 '{}' 失败：{}", name, e)))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    /// 名字与路径的区分规则必须简单且可预测——它决定了「数据写到哪」。
    #[test]
    fn names_are_distinguished_from_paths_by_the_separator() {
        assert!(looks_like_name("mydata"));
        assert!(looks_like_name("my-data_1"));
        // 含分隔符的一律是路径
        assert!(!looks_like_name("./data"));
        assert!(!looks_like_name("/var/data"));
        assert!(!looks_like_name("a/b"));
        assert!(!looks_like_name("a\\b"));
        // 明显是路径意图的单点/双点不能当卷名
        assert!(!looks_like_name("."));
        assert!(!looks_like_name(".."));
        assert!(!looks_like_name(""));
    }

    /// 非法名直接拒绝而不是「净化」：净化会让两个不同的名字落到同一个目录，
    /// 用户以为挂的是两个卷，其实是一个。
    #[test]
    fn illegal_names_are_rejected_not_sanitised() {
        let _home = TempHome::new("volname");
        assert!(dir_for("../escape").is_err());
        assert!(dir_for("a/b").is_err());
        assert!(dir_for("").is_err());
        assert!(dir_for("-f").is_err(), "以 - 开头会被当成选项");
        assert!(dir_for("ok").is_ok());
    }

    #[test]
    fn create_is_idempotent_and_reports_whether_it_was_new() {
        let _home = TempHome::new("volcreate");
        let (dir, fresh) = create("v1").unwrap();
        assert!(fresh, "第一次应是新建");
        assert!(dir.is_dir());
        let (dir2, fresh2) = create("v1").unwrap();
        assert_eq!(dir, dir2);
        assert!(!fresh2, "第二次是无操作，不该报新建");
        assert_eq!(list().unwrap(), vec!["v1"]);
    }

    #[test]
    fn remove_reports_missing_and_deletes_existing() {
        let _home = TempHome::new("volrm");
        assert!(remove("nope", false).is_err());
        create("v1").unwrap();
        // 卷里的内容要一并删掉，不能只删空壳
        std::fs::write(dir_for("v1").unwrap().join("f"), b"x").unwrap();
        remove("v1", false).unwrap();
        assert!(list().unwrap().is_empty());
    }

    /// 没有容器在跑时 `users` 为空——这条同时保证 `remove` 不会因为读不到
    /// 容器记录就误判成「有人在用」而拒绝。
    #[test]
    fn users_is_empty_without_running_containers() {
        let _home = TempHome::new("volusers");
        create("v1").unwrap();
        assert!(users("v1").is_empty());
        assert!(remove("v1", false).is_ok());
    }
}
