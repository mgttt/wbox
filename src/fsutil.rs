//! 文件系统清理的共用动作。
//!
//! # 为什么删目录要单独有个函数
//!
//! rootless overlayfs 会在 `workdir` 下建一个 **mode 000** 的 `work/work`。
//! 属主是我们自己，但 `0o000` 意味着连 `opendir` 都做不成——`rm -rf` 与
//! `std::fs::remove_dir_all` 都是先列目录再删，于是双双报 `Permission denied`，
//! 整棵树留在原地。
//!
//! **以 root 跑时看不见这个坑**：root 绕过权限位，`remove_dir_all` 直接成功。
//! 所以本地全绿、CI（普通用户）却留下 `.wbox-step-*` / `.wbox-stage-*` 残留，
//! 收尾的 `rm -rf` 还会因此非零退出把整条门禁带红。
//!
//! 修法就是删之前先把自己的 `rwx` 补回来。只补属主位、不跟符号链接——
//! 跟了就会顺着容器里放的链接去改宿主文件的权限。
use std::path::Path;

/// 递归删除，允许目标树里存在自己也进不去的目录。
///
/// 尽力而为、不报错：调用方要的都是"清理干净就好"，多一条错误分支没有价值。
pub fn remove_tree(path: &Path) {
    #[cfg(unix)]
    make_tree_owner_accessible(path);
    let _ = std::fs::remove_dir_all(path);
}

/// 把整棵树上属主缺失的 `rwx`/`rw` 位补回来（不跟符号链接）。
#[cfg(unix)]
pub fn make_tree_owner_accessible(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    // 符号链接本身没有有意义的权限位，而**跟过去改**就是在改链接指向的
    // 那个宿主文件——容器里放一条 `x -> /etc/shadow` 就能借此提权。
    if metadata.file_type().is_symlink() {
        return;
    }

    let required = if metadata.is_dir() { 0o700 } else { 0o600 };
    let mode = metadata.permissions().mode();
    if mode & required != required {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | required));
    }
    if !metadata.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        make_tree_owner_accessible(&entry.path());
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// overlayfs 的 `work/work` 是 mode 000：不先补权限就删不掉。
    #[cfg(unix)]
    #[test]
    fn removes_a_tree_containing_an_unreadable_directory() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("wbox-fsutil-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let locked = root.join("layer/work/work");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("index"), b"x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        // 先证明这个用例不是空跑：裸 remove_dir_all 在非 root 下会失败。
        // root 绕过权限位，那时只断言 remove_tree 的结果。
        remove_tree(&root);
        assert!(!root.exists(), "不可遍历的目录也必须删掉");
    }

    /// 补权限不能跟着符号链接走——跟了就是在改宿主文件。
    #[cfg(unix)]
    #[test]
    fn does_not_follow_symlinks_when_restoring_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("wbox-fsutil-ln-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let inner = root.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        let outside = root.join("canary");
        std::fs::write(&outside, b"keep").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::os::unix::fs::symlink(&outside, inner.join("link")).unwrap();

        make_tree_owner_accessible(&inner);
        assert_eq!(
            std::fs::symlink_metadata(&outside)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o000,
            "不该顺着链接把宿主文件的权限改掉"
        );
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
        remove_tree(&root);
    }
}
