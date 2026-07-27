//! fd 表与 VFS 路径翻译。
//!
//! guest 看到的是一个 Linux 根文件系统；宿主上它其实是 `prefix` 指向的一个目录。
//! 这一层负责把 guest 路径映射到宿主路径，并且**保证映射结果不逃出 prefix**
//! ——否则容器里的程序用 `../../` 就能读到宿主任意文件。
//!
//! 环境变量：`WBOX_PREFIX` 是首选；`BLINK_PREFIX` 作为兼容名保留，
//! 因为 `src/backend/blink.rs` 目前还在设它。

use std::collections::HashMap;
use std::fs::File;
use std::path::{Component, Path, PathBuf};

pub const PREFIX_ENV: &str = "WBOX_PREFIX";
pub const PREFIX_ENV_COMPAT: &str = "BLINK_PREFIX";

/// 一个打开的 guest 文件描述符背后的东西。
pub enum FdKind {
    /// 继承自宿主的标准流。
    Stdin,
    Stdout,
    Stderr,
    /// 普通文件。
    File(File),
    /// 打开的目录（`getdents64` 用）。
    Dir {
        path: PathBuf,
        /// 已缓存的目录项与游标；`getdents64` 分批返回。
        entries: Vec<(Vec<u8>, u8)>,
        pos: usize,
    },
    /// 已关闭但仍占位（`dup` 语义下的空洞）。
    Closed,
}

pub struct Fd {
    pub kind: FdKind,
    /// `O_CLOEXEC`：`execve` 时要关掉。
    pub cloexec: bool,
    /// 打开时的 flags，`fcntl(F_GETFL)` 要回它。
    pub flags: i32,
}

pub struct FdTable {
    map: HashMap<i32, Fd>,
    next: i32,
}

impl Default for FdTable {
    fn default() -> Self {
        Self::new()
    }
}

impl FdTable {
    pub fn new() -> Self {
        let mut map = HashMap::new();
        for (n, k) in [(0, FdKind::Stdin), (1, FdKind::Stdout), (2, FdKind::Stderr)] {
            map.insert(
                n,
                Fd {
                    kind: k,
                    cloexec: false,
                    flags: 0,
                },
            );
        }
        FdTable { map, next: 3 }
    }

    /// 分配最小可用 fd（Linux 保证的语义：总是最小的空号）。
    pub fn alloc(&mut self, fd: Fd) -> i32 {
        let mut n = 3;
        while self.map.contains_key(&n) {
            n += 1;
        }
        self.map.insert(n, fd);
        self.next = n + 1;
        n
    }

    /// 指定号插入（`dup2`）。
    pub fn insert_at(&mut self, n: i32, fd: Fd) {
        self.map.insert(n, fd);
    }

    pub fn get(&self, n: i32) -> Option<&Fd> {
        self.map.get(&n)
    }

    pub fn get_mut(&mut self, n: i32) -> Option<&mut Fd> {
        self.map.get_mut(&n)
    }

    pub fn remove(&mut self, n: i32) -> Option<Fd> {
        self.map.remove(&n)
    }

    pub fn contains(&self, n: i32) -> bool {
        self.map.contains_key(&n)
    }

    /// `execve` 时关掉带 `O_CLOEXEC` 的 fd。
    pub fn close_on_exec(&mut self) {
        self.map.retain(|_, f| !f.cloexec);
    }
}

/// VFS：guest 路径 <-> 宿主路径。
pub struct Vfs {
    /// guest `/` 对应的宿主目录。`None` 表示直通宿主根（无 rootfs 隔离）。
    pub prefix: Option<PathBuf>,
    /// guest 的当前工作目录（**guest 视角**的绝对路径）。
    pub cwd: PathBuf,
}

impl Vfs {
    pub fn from_env() -> Self {
        let prefix = std::env::var_os(PREFIX_ENV)
            .or_else(|| std::env::var_os(PREFIX_ENV_COMPAT))
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        // 设了 prefix 就是容器语义：guest 从自己的根开始。
        // 没设 prefix 是直通语义（guest / == 宿主 /），此时该像一个普通进程
        // 那样继承宿主的工作目录，否则相对路径全都对不上。
        let cwd = match &prefix {
            Some(_) => PathBuf::from("/"),
            None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
        };
        Vfs { prefix, cwd }
    }

    /// 把 guest 路径规范化成 guest 视角的绝对路径。
    ///
    /// 纯字符串运算，**不碰宿主文件系统**：`..` 在这里就地消掉，这样
    /// `/a/../../etc/passwd` 会归约到 `/etc/passwd` 而不是逃到 prefix 之外。
    /// 根之上的 `..` 被吃掉（和 Linux 对 `/..` 的处理一致）。
    pub fn normalize(&self, p: &str) -> PathBuf {
        let raw = Path::new(p);
        let joined: PathBuf = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.cwd.join(raw)
        };
        let mut out: Vec<std::ffi::OsString> = Vec::new();
        for c in joined.components() {
            match c {
                Component::RootDir | Component::Prefix(_) => out.clear(),
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                Component::Normal(s) => out.push(s.to_os_string()),
            }
        }
        let mut res = PathBuf::from("/");
        for s in out {
            res.push(s);
        }
        res
    }

    /// guest 路径 -> 宿主路径。
    ///
    /// 因为 `normalize` 已经把 `..` 全部消掉，拼接结果必然在 prefix 之内，
    /// 不需要再做 `canonicalize` 检查（那还会引入 TOCTOU）。
    ///
    /// **已知缺口**：宿主侧的 symlink 不做防护——rootfs 里若存在指向外部的
    /// 符号链接，guest 能顺着它出去。与 blink 的限制相同，见 crate 文档。
    pub fn host_path(&self, guest: &str) -> PathBuf {
        let norm = self.normalize(guest);
        match &self.prefix {
            None => norm,
            Some(pre) => {
                let rel = norm.strip_prefix("/").unwrap_or(&norm);
                pre.join(rel)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vfs(prefix: Option<&str>) -> Vfs {
        Vfs {
            prefix: prefix.map(PathBuf::from),
            cwd: PathBuf::from("/"),
        }
    }

    #[test]
    fn normalize_resolves_dot_and_dotdot() {
        let v = vfs(None);
        assert_eq!(v.normalize("/a/b/../c"), PathBuf::from("/a/c"));
        assert_eq!(v.normalize("/a/./b"), PathBuf::from("/a/b"));
        assert_eq!(v.normalize("/"), PathBuf::from("/"));
    }

    #[test]
    fn dotdot_cannot_escape_root() {
        let v = vfs(None);
        assert_eq!(v.normalize("/../../etc/passwd"), PathBuf::from("/etc/passwd"));
        assert_eq!(v.normalize("/a/../../.."), PathBuf::from("/"));
    }

    #[test]
    fn relative_paths_resolve_against_cwd() {
        let mut v = vfs(None);
        v.cwd = PathBuf::from("/usr/lib");
        assert_eq!(v.normalize("libc.so"), PathBuf::from("/usr/lib/libc.so"));
        assert_eq!(v.normalize("../bin/sh"), PathBuf::from("/usr/bin/sh"));
    }

    #[test]
    fn host_path_stays_inside_prefix() {
        let v = vfs(Some("/srv/rootfs"));
        assert_eq!(v.host_path("/bin/sh"), PathBuf::from("/srv/rootfs/bin/sh"));
        // 这是隔离的关键断言：任何 `..` 组合都不得跑到 prefix 之外
        assert_eq!(
            v.host_path("/../../../etc/shadow"),
            PathBuf::from("/srv/rootfs/etc/shadow")
        );
        assert_eq!(
            v.host_path("/bin/../../../../etc/shadow"),
            PathBuf::from("/srv/rootfs/etc/shadow")
        );
        assert!(v.host_path("/a/../../b").starts_with("/srv/rootfs"));
    }

    #[test]
    fn no_prefix_means_passthrough() {
        let v = vfs(None);
        assert_eq!(v.host_path("/etc/hosts"), PathBuf::from("/etc/hosts"));
    }

    #[test]
    fn fd_table_starts_with_std_streams() {
        let t = FdTable::new();
        assert!(matches!(t.get(0).unwrap().kind, FdKind::Stdin));
        assert!(matches!(t.get(1).unwrap().kind, FdKind::Stdout));
        assert!(matches!(t.get(2).unwrap().kind, FdKind::Stderr));
        assert!(t.get(3).is_none());
    }

    #[test]
    fn alloc_reuses_lowest_free_fd() {
        let mut t = FdTable::new();
        let a = t.alloc(Fd { kind: FdKind::Closed, cloexec: false, flags: 0 });
        let b = t.alloc(Fd { kind: FdKind::Closed, cloexec: false, flags: 0 });
        assert_eq!((a, b), (3, 4));
        t.remove(3);
        // Linux 保证下一个 open 拿到最小空号
        let c = t.alloc(Fd { kind: FdKind::Closed, cloexec: false, flags: 0 });
        assert_eq!(c, 3);
    }

    #[test]
    fn close_on_exec_drops_only_cloexec_fds() {
        let mut t = FdTable::new();
        let keep = t.alloc(Fd { kind: FdKind::Closed, cloexec: false, flags: 0 });
        let drop = t.alloc(Fd { kind: FdKind::Closed, cloexec: true, flags: 0 });
        t.close_on_exec();
        assert!(t.contains(keep));
        assert!(!t.contains(drop));
        assert!(t.contains(0), "标准流不带 CLOEXEC");
    }
}
