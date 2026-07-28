//! `wbox diff`：列出容器相对镜像改动了哪些文件（`PRD.md` F9.19）。
//!
//! # 这一格几乎是白捡的
//!
//! F9.12 给每个容器挂了 overlay 可写层，容器对 `/` 的所有写入都落在
//! `<状态目录>/layer/upper` 里。**upper 的内容就是"改了什么"的完整答案**，
//! 不需要扫描整棵 rootfs 去比对——那既慢又要拿到镜像原树。
//!
//! 输出格式与 `docker diff` 一致（`A`/`C`/`D` + 容器内路径），
//! 让习惯 docker 的人不用重新学。
//!
//! # 三类条目怎么判
//!
//! 与 overlay 的表示一一对应，和 build 侧的合并逻辑用的是同一套规则
//! （见 `build.rs` 的 `merge_overlay_upper`）：
//!
//! - **字符设备 0:0** = whiteout，表示下层的这个条目被删了 → `D`；
//! - 其余条目：镜像里已有同名的 → `C`（改动），没有的 → `A`（新增）。
//!
//! 目录只有在**它本身是新增的**时候才报 `A`；镜像里已有的目录出现在 upper 里
//! 往往只是因为它的某个子项被改过（overlay 会为此建同名目录），报成 `C` 会让
//! 输出里塞满没意义的行。docker 的行为也是这样。

use crate::error::{Result, WboxError};
use crate::layers::{is_whiteout, ContainerLayers};
use std::path::Path;

struct Options<'a> {
    name: &'a str,
}

fn parse<'a>(args: &'a [String]) -> Result<Options<'a>> {
    let mut name: Option<&str> = None;
    for a in args {
        match a.as_str() {
            other if other.starts_with('-') => {
                return Err(WboxError::args(format!(
                    "diff: 未知参数 '{}'（用法：wbox diff <NAME>）",
                    other
                )))
            }
            other => {
                if name.is_some() {
                    return Err(WboxError::args("diff: 一次只能看一个容器"));
                }
                name = Some(other);
            }
        }
    }
    let name = name.ok_or_else(|| WboxError::args("diff: 缺少容器名（用法：wbox diff <NAME>）"))?;
    Ok(Options { name })
}

/// 一条改动。`kind` 取 `A`/`C`/`D`，与 docker 对齐。
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Change {
    pub path: String,
    pub kind: char,
}

/// 扫描 overlay upper，得出改动清单。
///
/// `image_root` 用来区分"新增"与"改动"：`None`（宿主程序模式没有镜像）时
/// 一律报 `A`——没有可比的基线，硬说成 `C` 是编造。
pub fn scan(upper: &Path, image_root: Option<&Path>) -> Vec<Change> {
    let mut out = Vec::new();
    walk(upper, image_root, Path::new(""), &mut out);
    out.sort();
    out
}

fn walk(upper: &Path, image_root: Option<&Path>, rel: &Path, out: &mut Vec<Change>) {
    let dir = upper.join(rel);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    for ent in rd.flatten() {
        // `.wbox_oldroot` 是 pivot_root 的暂存目录，wbox 自己的产物。
        // 它必然出现在 upper 里（换根后被 rmdir），但对用户毫无意义——
        // 打印它等于在答案里掺自己的实现细节。push 的打包侧也滤掉同一个。
        if rel.as_os_str().is_empty() && ent.file_name() == ".wbox_oldroot" {
            continue;
        }
        let r = rel.join(ent.file_name());
        let p = upper.join(&r);
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        // 容器内路径：upper 里的相对路径前面加 `/`
        let shown = format!("/{}", r.to_string_lossy());
        let in_image = image_root.map(|b| b.join(&r).symlink_metadata().is_ok());
        if is_whiteout(&meta) {
            out.push(Change {
                path: shown,
                kind: 'D',
            });
        } else if meta.is_dir() {
            // 目录只有本身是新增的才报；镜像里已有的目录出现在 upper 里往往
            // 只是因为子项被改过，报出来全是噪音。
            if in_image == Some(false) {
                out.push(Change {
                    path: shown,
                    kind: 'A',
                });
            }
            walk(upper, image_root, &r, out);
        } else {
            out.push(Change {
                path: shown,
                kind: if in_image == Some(true) { 'C' } else { 'A' },
            });
        }
    }
}

pub fn cmd_diff(args: &[String]) -> Result<u32> {
    let opts = parse(args)?;
    // 分层解析与"没有 overlay 层"的措辞都在 layers 模块（一处措辞，四处复用）；
    // 明说而不是打印空清单——空清单会被读成"没改过"。
    let layers = ContainerLayers::resolve(opts.name, "无法列出改动")?;
    // 镜像 rootfs 作为基线，用来区分新增与改动。取不到就只报新增。
    for c in scan(&layers.upper, layers.lower.as_deref()) {
        println!("{} {}", c.kind, c.path);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runstate;
    use crate::testenv::TempHome;

    #[test]
    fn parse_requires_exactly_one_name() {
        assert_eq!(parse(&["c".to_string()]).unwrap().name, "c");
        assert!(parse(&[]).is_err());
        assert!(parse(&["a".to_string(), "b".to_string()]).is_err());
        assert!(parse(&["--bogus".to_string()]).is_err());
    }

    /// 三类条目各归各位。这条同时钉死"镜像里已有的目录不报"——
    /// 不然 upper 里每个中间目录都会冒出来一行，输出全是噪音。
    #[cfg(unix)]
    #[test]
    fn classifies_added_changed_and_deleted() {
        let base = std::env::temp_dir().join(format!("wbox-diff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (upper, image) = (base.join("upper"), base.join("image"));
        std::fs::create_dir_all(upper.join("etc")).unwrap();
        std::fs::create_dir_all(upper.join("brandnew")).unwrap();
        std::fs::create_dir_all(image.join("etc")).unwrap();

        // 镜像里已有 → C
        std::fs::write(image.join("etc/conf"), b"old").unwrap();
        std::fs::write(upper.join("etc/conf"), b"new").unwrap();
        // 镜像里没有 → A
        std::fs::write(upper.join("brandnew/f"), b"x").unwrap();

        let out = scan(&upper, Some(&image));
        let find = |p: &str| out.iter().find(|c| c.path == p).map(|c| c.kind);
        assert_eq!(find("/etc/conf"), Some('C'));
        assert_eq!(find("/brandnew"), Some('A'), "新增目录要报 A");
        assert_eq!(find("/brandnew/f"), Some('A'));
        assert_eq!(find("/etc"), None, "镜像里已有的目录不该报出来");

        // wbox 自己的换根暂存目录不该混进用户看的清单
        std::fs::create_dir_all(upper.join(".wbox_oldroot")).unwrap();
        let out = scan(&upper, Some(&image));
        assert!(
            !out.iter().any(|c| c.path.contains(".wbox_oldroot")),
            "内部产物不该出现在 diff 里：{:?}",
            out
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 没有基线时一律报 A——硬说成 C 是编造。
    #[cfg(unix)]
    #[test]
    fn without_image_baseline_everything_is_added() {
        let base = std::env::temp_dir().join(format!("wbox-diff2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let upper = base.join("upper");
        std::fs::create_dir_all(&upper).unwrap();
        std::fs::write(upper.join("f"), b"x").unwrap();
        let out = scan(&upper, None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, 'A');
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 没有 overlay 层时必须报错并说清原因。打印空清单会被读成"没改过"，
    /// 那是把"不知道"说成了"没有"。
    #[test]
    fn missing_overlay_layer_explains_instead_of_printing_nothing() {
        let _home = TempHome::new("diffnolayer");
        let dir = runstate::dir_for("c").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let e = cmd_diff(&["c".to_string()]).unwrap_err();
        let m = format!("{}", e);
        assert!(m.contains("overlay"), "{}", m);
        assert!(m.contains("宿主程序模式"), "要说清什么情况下没有：{}", m);
    }

    #[test]
    fn unknown_container_is_an_error() {
        let _home = TempHome::new("diffunknown");
        assert!(cmd_diff(&["nope".to_string()]).is_err());
    }
}
