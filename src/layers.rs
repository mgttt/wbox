//! 容器的**分层视图**：上层是该容器的 overlay upper，下层是共享只读的镜像
//! rootfs（`PRD.md` F9.12）。
//!
//! # 为什么单独成一个模块
//!
//! `diff`（F9.19）、`commit`（F9.20）、`cp`（F9.23）、`export`（F9.25）问的
//! 是同一个问题——"这个容器的文件系统由哪两层构成"——此前各自解析了一遍，
//! 连"没有 overlay 可写层"那句错误说明都各抄了一份三处。抄出来的东西改一处
//! 就会漏两处，而漏掉的那两处不会有任何提示。
//!
//! 于是把这件事收敛到 [`ContainerLayers::resolve`]：一处解析、一处措辞，
//! 调用方只需说清"我拿这个视图要干什么"（`why`），错误里就带上它。
//!
//! # 两条派生能力
//!
//! - [`ContainerLayers::lookup`]：在分层里定位一个容器内路径（读先 upper 后
//!   lower，认 whiteout）。`cp` 用它取文件。
//! - [`ContainerLayers::materialize`]：把合并后的完整 rootfs 物化到一个目录
//!   （硬链接铺下层 + 合并 upper）。`commit` 和 `export` 都用它。

use crate::error::{Result, WboxError};
use std::path::{Path, PathBuf};

/// 一个容器的分层视图。
#[derive(Debug, Clone)]
pub struct ContainerLayers {
    /// 该容器的 overlay 可写层。
    pub upper: PathBuf,
    /// 镜像 rootfs（overlay 的 lowerdir）。宿主程序模式没有镜像时为 `None`。
    pub lower: Option<PathBuf>,
}

impl ContainerLayers {
    /// 解析容器的分层。`why` 说明拿这个视图要做什么，会出现在错误里——
    /// "没有 overlay 层"本身不足以让用户知道该怎么办，得说清是哪件事做不成。
    pub fn resolve(container: &str, why: &str) -> Result<Self> {
        let dir = crate::runstate::resolve_existing(container)?;
        Self::from_state_dir(&dir, container, why)
    }

    /// 已经拿到状态目录时的入口（`commit` 走这条，它还要用同一个目录取别的东西）。
    pub fn from_state_dir(dir: &Path, container: &str, why: &str) -> Result<Self> {
        let upper = dir.join("layer").join("upper");
        if !upper.is_dir() {
            // 静默给空结果比报错糟得多：空清单会被读成"没有改动"，
            // 空文件会被读成"容器里就是这样"——都是把"不知道"说成了"没有"。
            return Err(WboxError::args(format!(
                "容器 '{}' 没有 overlay 可写层，{}：\
                 宿主程序模式不换根，或本机内核不支持 rootless overlay 而回退了共享写入（PRD F9.12）",
                container, why
            )));
        }
        let lower = crate::runstate::read_meta(dir)
            .and_then(|m| m.exec_context.map(|c| PathBuf::from(c.workdir)));
        Ok(Self { upper, lower })
    }

    /// 镜像目录（rootfs 的父目录）。取不到镜像下层时为 `None`。
    ///
    /// 只有 `commit` 用得上（要读基础镜像的 config 和层清单），而 commit 本身
    /// 是 `cfg(not(windows))` 的，故在 Windows 目标上这个方法确实没有调用者。
    #[cfg_attr(windows, allow(dead_code))]
    pub fn image_dir(&self) -> Option<&Path> {
        self.lower.as_deref().and_then(|r| r.parent())
    }

    /// 在分层视图里定位一个容器内绝对路径。
    ///
    /// 顺序是 upper → lower，且**必须认 whiteout**：容器删掉的文件在 upper 里
    /// 是字符设备 0:0，不认它就会把下层那份早已被删掉的旧文件当成容器现状交出去。
    pub fn lookup(&self, guest: &str) -> Result<PathBuf> {
        let up = crate::build::resolve_rootfs_path(&self.upper, guest)?;
        if let Ok(meta) = std::fs::symlink_metadata(&up) {
            if is_whiteout(&meta) {
                return Err(WboxError::args(format!(
                    "容器内 '{}' 已被删除（overlay whiteout）",
                    guest
                )));
            }
            return Ok(up);
        }
        let low = self.lower.as_deref().ok_or_else(|| {
            WboxError::args(format!("容器内找不到 '{}'（且该容器没有镜像下层）", guest))
        })?;
        let lp = crate::build::resolve_rootfs_path(low, guest)?;
        if lp.symlink_metadata().is_ok() {
            return Ok(lp);
        }
        Err(WboxError::args(format!("容器内找不到 '{}'", guest)))
    }

    /// 把合并后的完整 rootfs 物化到 `dst`。
    ///
    /// 下层用硬链接铺开（磁盘不翻倍，F9.18），再把 upper 合并上去
    /// （whiteout/opaque 一并处理）。合并侧**先 unlink 再落盘**，所以与基础
    /// 镜像共享 inode 的那些文件不会被就地改写——这条纪律破了就会写坏别的镜像。
    #[cfg(not(windows))]
    pub fn materialize(&self, dst: &Path) -> Result<()> {
        if let Some(lower) = &self.lower {
            crate::build::link_tree(lower, dst)?;
        } else {
            std::fs::create_dir_all(dst)
                .map_err(|e| WboxError::args(format!("创建目录失败：{}", e)))?;
        }
        crate::build::merge_overlay_upper(&self.upper, dst)?;
        // 换根暂存目录是 wbox 自己的产物，不属于容器内容
        // （diff / push 的打包侧滤掉的是同一个）。
        let _ = std::fs::remove_dir_all(dst.join(".wbox_oldroot"));
        Ok(())
    }
}

/// overlay 用字符设备 0:0 表示"下层的这个条目被删了"。
/// 与 tar 层的 `.wh.` 前缀**不是**一套（见 `build.rs` 的同名说明）。
pub fn is_whiteout(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        use std::os::unix::fs::MetadataExt;
        meta.file_type().is_char_device() && meta.rdev() == 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    /// 没有 overlay 层时必须报错，且错误里要带上调用方说的"要干什么"——
    /// 光说"没有 overlay 层"，用户不知道是哪件事做不成。
    #[test]
    fn missing_layer_error_carries_the_caller_intent() {
        let _home = TempHome::new("layersnolayer");
        let dir = crate::runstate::dir_for("c").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let m = format!(
            "{}",
            ContainerLayers::resolve("c", "无法列出改动").unwrap_err()
        );
        assert!(m.contains("overlay"), "{}", m);
        assert!(m.contains("无法列出改动"), "要带上调用方的意图：{}", m);
        assert!(m.contains("宿主程序模式"), "要说清什么情况下没有：{}", m);
    }

    /// upper 遮住 lower；两层都没有要报错而不是返回一个不存在的路径。
    #[cfg(unix)]
    #[test]
    fn lookup_prefers_upper_then_lower() {
        let base = std::env::temp_dir().join(format!("wbox-layers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (upper, lower) = (base.join("upper"), base.join("lower"));
        std::fs::create_dir_all(&upper).unwrap();
        std::fs::create_dir_all(&lower).unwrap();
        std::fs::write(lower.join("f"), b"old").unwrap();
        let l = ContainerLayers {
            upper: upper.clone(),
            lower: Some(lower.clone()),
        };
        assert_eq!(l.lookup("/f").unwrap(), lower.join("f"));
        std::fs::write(upper.join("f"), b"new").unwrap();
        assert_eq!(l.lookup("/f").unwrap(), upper.join("f"));
        assert!(l.lookup("/nope").is_err());

        let only_upper = ContainerLayers { upper, lower: None };
        assert!(only_upper.lookup("/nope").is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 物化出来的是**合并后**的视图：下层的文件在、upper 改过的以 upper 为准、
    /// upper 删掉的不能复活。
    #[cfg(all(unix, not(windows)))]
    #[test]
    fn materialize_produces_the_merged_view() {
        let base = std::env::temp_dir().join(format!("wbox-layersm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (upper, lower, out) = (base.join("upper"), base.join("lower"), base.join("out"));
        std::fs::create_dir_all(&upper).unwrap();
        std::fs::create_dir_all(&lower).unwrap();
        std::fs::write(lower.join("keep"), b"base").unwrap();
        std::fs::write(lower.join("changed"), b"base").unwrap();
        std::fs::write(upper.join("changed"), b"new").unwrap();
        std::fs::write(upper.join("added"), b"x").unwrap();
        // 内部产物不该出现在物化结果里
        std::fs::create_dir_all(upper.join(".wbox_oldroot")).unwrap();

        let l = ContainerLayers {
            upper,
            lower: Some(lower.clone()),
        };
        l.materialize(&out).unwrap();
        assert_eq!(std::fs::read(out.join("keep")).unwrap(), b"base");
        assert_eq!(std::fs::read(out.join("changed")).unwrap(), b"new");
        assert!(out.join("added").exists());
        assert!(
            !out.join(".wbox_oldroot").exists(),
            "内部产物不该留在物化结果里"
        );
        // 合并必须**先 unlink 再落盘**：下层那份不能被就地改写，
        // 否则会写坏与它共享 inode 的别的镜像。
        assert_eq!(
            std::fs::read(lower.join("changed")).unwrap(),
            b"base",
            "下层被就地改写了"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
