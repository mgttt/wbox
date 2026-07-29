//! rootfs 内的路径解析——`openat2(RESOLVE_IN_ROOT)` 的用户态版本。
//!
//! # 为什么要有这个模块
//!
//! 「把一个**容器内**的绝对路径翻译成宿主路径」在本仓出现了三次，而三次都是
//! 同一个洞的入口：路径词法上合法，却经由**符号链接**指到了别处。
//!
//! - **L7 归档解包**：tar 条目 `a/b` 里 `a` 是链接。用的是
//!   [`crate::tar::safe_join`]——那里的策略是**拒绝**，因为归档是完全不可信的
//!   外部输入，"跟着链接走"没有任何合理语义。
//! - **L12 guest 运行期**：`crates/wbox-linux` 的 VFS。策略是**跟随并重新以
//!   rootfs 为根展开**——guest 里的 `/etc -> /usr/etc` 就该在容器内生效。
//! - **L8 `cp` / `COPY` / `ADD`**：本模块。与 L12 同类，同一套策略。
//!
//! 早先 L8 这一路是**纯词法**的：只逐段消解 `..`，从不看符号链接。于是镜像里
//! 放一个 `/evil -> /home/someone`（宿主绝对路径），`COPY x /evil/y` 就会在
//! **宿主**上落文件——镜像是外部输入，这是一条实打实的越界写。
//!
//! # 策略
//!
//! 逐段解析，符号链接**重新从 root 展开**：链接目标是绝对路径就清空已解析栈
//! （等于回到 rootfs 根），相对路径就接着当前位置走。`..` 在**解析栈**上生效，
//! 不能在入队时就地消掉——就地消掉正是旧实现挡不住链接的原因。

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// 符号链接展开的次数上限。超过就当成环。
///
/// 40 是 Linux 的 `MAXSYMLINKS`，与 `crates/wbox-linux` 的 VFS 取同一个值。
pub const MAX_SYMLINK_DEPTH: u32 = 40;

/// 解析失败的两种成因。调用方要分开报——它们对用户是两件事。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveError {
    /// `..` 在根上还要往上，逃出了 rootfs。
    Escaped,
    /// 符号链接成环（或嵌套过深）。
    Loop,
}

fn is_guest_absolute(p: &Path) -> bool {
    // guest 路径一律用 `/`；在 Windows 宿主上 `is_absolute()` 认不出 `/etc`，
    // 所以先看首字符。
    p.to_string_lossy().starts_with('/') || p.is_absolute()
}

fn push_segments(p: &Path, out: &mut VecDeque<OsString>) {
    for c in p.components() {
        match c {
            Component::RootDir | Component::Prefix(_) | Component::CurDir => {}
            Component::ParentDir => out.push_back(OsString::from("..")),
            Component::Normal(s) => out.push_back(s.to_os_string()),
        }
    }
}

fn join_under(root: &Path, stack: &[OsString]) -> PathBuf {
    let mut out = root.to_path_buf();
    for s in stack {
        out.push(s);
    }
    out
}

/// 把 `guest` 解析成 `root` 之下的宿主路径，保证结果不逃出 `root`。
///
/// `follow_final` 决定末段的符号链接是否展开：`true` 用于"要读/写链接指向的
/// 那个东西"（`COPY` 目标、`cp` 目标——底层的 `File::create` 本来也会跟随），
/// `false` 用于"要操作链接本身"。
///
/// 路径不存在**不是错误**：目标通常还没被创建。一旦某一段不存在，后面的段就
/// 不可能是链接，原样拼上即可——`..` 仍然作用在解析栈上，照样逃不出去。
pub fn resolve_in_root(
    root: &Path,
    guest: &Path,
    follow_final: bool,
) -> Result<PathBuf, ResolveError> {
    let mut stack: Vec<OsString> = Vec::new();
    let mut pending: VecDeque<OsString> = VecDeque::new();
    push_segments(guest, &mut pending);

    let mut links = 0u32;
    while let Some(seg) = pending.pop_front() {
        if seg == ".." {
            if stack.pop().is_none() {
                return Err(ResolveError::Escaped);
            }
            continue;
        }
        stack.push(seg);

        // 末段是否展开由调用方定；中间段一律展开。
        if pending.is_empty() && !follow_final {
            break;
        }
        let here = join_under(root, &stack);
        let Ok(md) = std::fs::symlink_metadata(&here) else {
            continue; // 不存在或看不到：不再往下解析
        };
        if !md.file_type().is_symlink() {
            continue;
        }
        links += 1;
        if links > MAX_SYMLINK_DEPTH {
            return Err(ResolveError::Loop);
        }
        let Ok(target) = std::fs::read_link(&here) else {
            continue;
        };
        stack.pop();
        if is_guest_absolute(&target) {
            // **guest 里的绝对链接以 rootfs 为根**，不是宿主根。
            stack.clear();
        }
        let mut expanded = VecDeque::new();
        push_segments(&target, &mut expanded);
        while let Some(x) = expanded.pop_back() {
            pending.push_front(x);
        }
    }

    Ok(join_under(root, &stack))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("wbox-codec-path-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(unix)]
    fn link(target: &str, at: &Path) {
        std::os::unix::fs::symlink(target, at).unwrap();
    }

    #[test]
    fn plain_paths_resolve_under_root() {
        let root = tmp("plain");
        assert_eq!(
            resolve_in_root(&root, Path::new("/app/x"), true).unwrap(),
            root.join("app").join("x")
        );
        // `..` 在栈上生效
        assert_eq!(
            resolve_in_root(&root, Path::new("/app/../x"), true).unwrap(),
            root.join("x")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parent_dir_above_root_is_rejected() {
        let root = tmp("esc");
        assert_eq!(
            resolve_in_root(&root, Path::new("/../escape"), true),
            Err(ResolveError::Escaped)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 这条是 L8 的核心：镜像里的**绝对**链接必须以 rootfs 为根重新展开，
    /// 不能落到宿主上。纯词法的旧实现在这里会返回 `<root>/evil/y`，
    /// 而宿主随后跟着 `evil` 走到 `/tmp/...outside`。
    #[cfg(unix)]
    #[test]
    fn absolute_symlink_is_reanchored_at_root() {
        let root = tmp("abs");
        let outside = tmp("abs-outside");
        link(&outside.to_string_lossy(), &root.join("evil"));
        let got = resolve_in_root(&root, Path::new("/evil/y"), true).unwrap();
        assert!(
            got.starts_with(&root),
            "解析结果逃出了 root：{}",
            got.display()
        );
        // 链接目标 `/tmp/xxx` 被当成 guest 绝对路径，从 root 重新展开
        assert!(
            !got.starts_with(&outside),
            "落到宿主目录：{}",
            got.display()
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// 相对链接照常跟随，但 `..` 只能在栈上退到根为止。
    #[cfg(unix)]
    #[test]
    fn relative_symlink_cannot_climb_out() {
        let root = tmp("rel");
        std::fs::create_dir_all(root.join("a")).unwrap();
        link("../../..", &root.join("a").join("up"));
        assert_eq!(
            resolve_in_root(&root, Path::new("/a/up/x"), true),
            Err(ResolveError::Escaped)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 正常的镜像内链接要继续工作——只挡逃逸，不许顺手把功能也挡了。
    #[cfg(unix)]
    #[test]
    fn in_image_symlink_still_follows() {
        let root = tmp("inimg");
        std::fs::create_dir_all(root.join("usr").join("etc")).unwrap();
        link("/usr/etc", &root.join("etc"));
        assert_eq!(
            resolve_in_root(&root, Path::new("/etc/conf"), true).unwrap(),
            root.join("usr").join("etc").join("conf")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loops_report_loop_not_escape() {
        let root = tmp("loop");
        link("b", &root.join("a"));
        link("a", &root.join("b"));
        assert_eq!(
            resolve_in_root(&root, Path::new("/a"), true),
            Err(ResolveError::Loop)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `follow_final = false`：末段是链接时保留链接本身。
    #[cfg(unix)]
    #[test]
    fn no_follow_keeps_the_final_link() {
        let root = tmp("nofollow");
        std::fs::create_dir_all(root.join("real")).unwrap();
        link("/real", &root.join("l"));
        assert_eq!(
            resolve_in_root(&root, Path::new("/l"), false).unwrap(),
            root.join("l")
        );
        assert_eq!(
            resolve_in_root(&root, Path::new("/l"), true).unwrap(),
            root.join("real")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 不存在的路径不是错误：`COPY` 的目标通常还没被创建。
    #[test]
    fn missing_paths_resolve_without_error() {
        let root = tmp("missing");
        assert_eq!(
            resolve_in_root(&root, Path::new("/nope/deep/x"), true).unwrap(),
            root.join("nope").join("deep").join("x")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
