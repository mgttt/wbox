//! `wbox cp`：在宿主与容器之间拷贝文件（`PRD.md` F9.23）。
//!
//! # 靠分层视图，不进容器
//!
//! 容器的文件系统是 overlay：**下层**是镜像 rootfs（只读共享），
//! **上层**是该容器的 `layer/upper`。宿主侧不必 `setns` 进容器就能取到同一份
//! 视图——读的时候先看 upper 再看 lower，写的时候只写 upper。
//!
//! 这样做的两个好处：拷贝不需要容器**正在运行**（已退出但未 `rm` 的容器照样能
//! 取文件，排查失败容器时这恰恰是最需要的）；写进去的东西对运行中的容器
//! **立即可见**，因为 overlay 是活的。
//!
//! # 删除标记要认
//!
//! 容器删过的文件在 upper 里是字符设备 0:0（whiteout）。**必须认它**：
//! 否则会把镜像里那份"已经被删掉的"旧文件拷出去，而用户以为拿到的是容器现状。

use crate::error::{Result, WboxError};
use crate::layers::ContainerLayers;
use crate::runstate;
use std::path::{Path, PathBuf};

/// 拷贝方向。
#[derive(Debug)]
enum Direction {
    /// 容器 → 宿主
    Out { container: String, guest: String, host: PathBuf },
    /// 宿主 → 容器
    In { container: String, host: PathBuf, guest: String },
}

/// 判断一个参数是不是 `<容器>:<路径>` 形式。
///
/// 判据是**冒号前那段确实是个已登记的容器**，而不是"含冒号"——
/// `./a:b` 是合法的宿主文件名，按含冒号来判会把它误当成容器。
fn split_container(arg: &str) -> Option<(String, String)> {
    let (name, path) = arg.split_once(':')?;
    if name.is_empty() || path.is_empty() {
        return None;
    }
    runstate::resolve_existing(name).ok()?;
    Some((name.to_string(), path.to_string()))
}

fn parse(args: &[String]) -> Result<Direction> {
    let pos: Vec<&String> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .collect();
    if pos.len() != args.len() {
        return Err(WboxError::args(
            "cp: 暂不支持选项（用法：wbox cp <容器>:<路径> <宿主路径> 或反向）",
        ));
    }
    if pos.len() != 2 {
        return Err(WboxError::args(
            "cp 需要两个参数：<容器>:<路径> <宿主路径>，或 <宿主路径> <容器>:<路径>",
        ));
    }
    let (a, b) = (pos[0].as_str(), pos[1].as_str());
    match (split_container(a), split_container(b)) {
        (Some(_), Some(_)) => Err(WboxError::args(
            "cp: 两端都是容器路径；容器之间的直接拷贝尚未支持，请经宿主中转",
        )),
        (Some((container, guest)), None) => Ok(Direction::Out {
            container,
            guest,
            host: PathBuf::from(b),
        }),
        (None, Some((container, guest))) => Ok(Direction::In {
            container,
            host: PathBuf::from(a),
            guest,
        }),
        // 没认出容器：多半是名字写错或容器没登记，直说而不是当成两个宿主路径
        (None, None) => Err(WboxError::args(
            "cp: 有一端必须是 <容器>:<路径>；若已写了容器名，请确认它出现在 `wbox ps -a` 里",
        )),
    }
}

pub fn cmd_cp(args: &[String]) -> Result<u32> {
    match parse(args)? {
        Direction::Out {
            container,
            guest,
            host,
        } => copy_out(&container, &guest, &host),
        Direction::In {
            container,
            host,
            guest,
        } => copy_in(&container, &host, &guest),
    }
}

fn copy_out(container: &str, guest: &str, host: &Path) -> Result<u32> {
    let layers = ContainerLayers::resolve(container, "cp 无法取得它的文件系统视图")?;
    let src = layers.lookup(guest)?;
    // 目标是已存在的目录时，按 cp 的习惯拷进去而不是覆盖它
    let dst = if host.is_dir() {
        host.join(src.file_name().unwrap_or_default())
    } else {
        host.to_path_buf()
    };
    copy_any(&src, &dst)?;
    println!("{} → {}", guest, dst.display());
    Ok(0)
}

fn copy_in(container: &str, host: &Path, guest: &str) -> Result<u32> {
    if !host.exists() {
        return Err(WboxError::args(format!(
            "宿主路径 '{}' 不存在",
            host.display()
        )));
    }
    let layers = ContainerLayers::resolve(container, "cp 无法取得它的文件系统视图")?;
    // 写只写 upper：镜像下层是共享只读的，往那里写会污染别的容器（F9.12）
    let mut dst = crate::build::resolve_rootfs_path(&layers.upper, guest)?;
    // 目标以 `/` 结尾、或容器内已是目录时，拷进去而不是覆盖
    if guest.ends_with('/') || dst.is_dir() {
        dst = dst.join(host.file_name().unwrap_or_default());
    }
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p)
            .map_err(|e| WboxError::args(format!("创建容器内目录失败：{}", e)))?;
    }
    copy_any(host, &dst)?;
    println!("{} → {}:{}", host.display(), container, guest);
    Ok(0)
}

/// 拷文件或整棵目录。**先删再写**：目标可能是与镜像缓存共享 inode 的硬链接
/// （F9.18），就地写会改到别的镜像。
fn copy_any(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(src)
        .map_err(|e| WboxError::args(format!("读取 '{}' 失败：{}", src.display(), e)))?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst)
            .map_err(|e| WboxError::args(format!("创建 '{}' 失败：{}", dst.display(), e)))?;
        let rd = std::fs::read_dir(src)
            .map_err(|e| WboxError::args(format!("读取目录 '{}' 失败：{}", src.display(), e)))?;
        for e in rd {
            let e = e.map_err(|e| WboxError::args(format!("枚举目录项失败：{}", e)))?;
            copy_any(&e.path(), &dst.join(e.file_name()))?;
        }
        return Ok(());
    }
    let _ = std::fs::remove_file(dst);
    #[cfg(unix)]
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(src)
            .map_err(|e| WboxError::args(format!("读取链接失败：{}", e)))?;
        std::os::unix::fs::symlink(&target, dst)
            .map_err(|e| WboxError::args(format!("创建链接失败：{}", e)))?;
        return Ok(());
    }
    std::fs::copy(src, dst)
        .map(|_| ())
        .map_err(|e| WboxError::args(format!("拷贝到 '{}' 失败：{}", dst.display(), e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn parse_rejects_malformed_argument_shapes() {
        let _home = TempHome::new("cpparse");
        assert!(parse(&[]).is_err());
        assert!(parse(&["a".to_string()]).is_err());
        assert!(parse(&["a".to_string(), "b".to_string(), "c".to_string()]).is_err());
        assert!(parse(&["-x".to_string(), "b".to_string()]).is_err());
        // 两端都不是容器：说清要写 <容器>:<路径>
        let m = format!(
            "{}",
            parse(&["a".to_string(), "b".to_string()]).unwrap_err()
        );
        assert!(m.contains("ps -a"), "要提示怎么确认容器名：{}", m);
    }

    /// 判定容器端要看"冒号前是不是真的容器"，不能只看含不含冒号——
    /// `./a:b` 是合法的宿主文件名。
    #[test]
    fn colon_alone_does_not_make_it_a_container() {
        let _home = TempHome::new("cpcolon");
        assert!(split_container("nosuch:/x").is_none());
        let dir = runstate::dir_for("real").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            split_container("real:/etc/x"),
            Some(("real".to_string(), "/etc/x".to_string()))
        );
    }

    /// 没有 overlay 层时报错并说清原因；静默拷镜像的内容会让用户以为那是容器现状。
    ///
    /// 分层解析本身（upper 遮 lower、whiteout、两层都没有）由 `crate::layers`
    /// 的单测覆盖——那是它的职责，在这里再抄一遍只会有两份要一起改的断言。
    #[test]
    fn missing_overlay_layer_is_explained() {
        let _home = TempHome::new("cpnolayer");
        let dir = runstate::dir_for("c").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let m = format!("{}", copy_out("c", "/x", Path::new("/tmp/x")).unwrap_err());
        assert!(m.contains("overlay"), "{}", m);
        assert!(m.contains("cp 无法取得"), "错误里要说清是哪件事做不成：{}", m);
    }
}
