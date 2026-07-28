//! fd 表与 VFS 路径翻译。
//!
//! guest 看到的是一个 Linux 根文件系统；宿主上它其实是 `prefix` 指向的一个目录。
//! 这一层负责把 guest 路径映射到宿主路径，并且**保证映射结果不逃出 prefix**
//! ——否则容器里的程序用 `../../` 就能读到宿主任意文件。
//!
//! 环境变量：`WBOX_PREFIX` 是首选；`BLINK_PREFIX` 作为兼容名保留，
//! 因为 `src/backend/blink.rs` 目前还在设它。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

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
    /// 匿名管道的读端。
    PipeRead(Rc<PipeInner>),
    /// 匿名管道的写端。持有 `PipeWriter` 而不是裸 `Rc`，这样"还有几个写端
    /// 开着"是自动记账的，见 `PipeWriter`。
    PipeWrite(PipeWriter),
    /// 合成的字符设备（`/dev/null` 等），见 `DevKind`。
    Dev(DevKind),
    /// 已关闭但仍占位（`dup` 语义下的空洞）。
    Closed,
}

/// `/dev` 下我们**自己合成**的字符设备。
///
/// 为什么必须合成而不是转给宿主：容器的 rootfs 里通常没有 `/dev`，而
/// `2>/dev/null` 是 shell 脚本里最常见的一句；Windows 宿主上更是根本没有
/// 这些路径。少了它们，绝大多数真实脚本第一行就挂。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevKind {
    /// `/dev/null`：读到 EOF，写入丢弃。
    Null,
    /// `/dev/zero`：读到无限的 0，写入丢弃。
    Zero,
    /// `/dev/full`：读到无限的 0，写入一律 `ENOSPC`。
    Full,
    /// `/dev/random` / `/dev/urandom`：读到随机字节。
    Random,
    /// `/dev/tty`：转给宿主的标准流（读 stdin、写 stdout）。
    Tty,
}

impl DevKind {
    /// 按 **guest 绝对路径**识别。只认绝对路径：相对路径要求我们合成出
    /// `/dev` 这个目录本身（`getdents64` 能列出来），那是另一件事，
    /// 没做就不要假装做了。
    pub fn from_guest_path(p: &str) -> Option<DevKind> {
        Some(match p {
            "/dev/null" => DevKind::Null,
            "/dev/zero" => DevKind::Zero,
            "/dev/full" => DevKind::Full,
            "/dev/random" | "/dev/urandom" => DevKind::Random,
            "/dev/tty" | "/dev/console" => DevKind::Tty,
            "/dev/stdin" => DevKind::Tty,
            _ => return None,
        })
    }
}

/// 匿名管道的共享状态。
///
/// 缓冲区在**宿主进程内**共享（`Rc`）。快照式 `fork`（见 `syscall/process.rs`）
/// 下父子在同一个宿主进程里，所以 fork 出来的两端指向同一个 `PipeInner`，
/// `echo x | cat` 这种"写完再读"的用法能通。
pub struct PipeInner {
    pub data: RefCell<VecDeque<u8>>,
    /// 当前还开着的**写端**个数。读到空缓冲时要靠它区分两件事：
    /// 还有人可能写（`EAGAIN`）vs. 写端全关了（返回 0，也就是 EOF）。
    /// 没有这个计数，`$(cmd)` 会在读端上无限 `EAGAIN` 自旋。
    writers: Cell<usize>,
}

impl PipeInner {
    fn new() -> Rc<Self> {
        Rc::new(PipeInner {
            data: RefCell::new(VecDeque::new()),
            writers: Cell::new(0),
        })
    }

    /// 写端是否已全部关闭（读端据此报 EOF）。
    pub fn writers_closed(&self) -> bool {
        self.writers.get() == 0
    }
}

/// 写端的持有凭证：构造 +1、`Drop` -1。
///
/// 用 RAII 而不是在 `close` 里手工减，是因为 fd 表消失的路径有好几条
/// （`close`、`dup2` 覆盖、`execve` 清 `O_CLOEXEC`、子进程退出整表析构），
/// 手工记账一定会漏，而漏账的表现是读端永远读不到 EOF——挂死。
pub struct PipeWriter(Rc<PipeInner>);

impl PipeWriter {
    fn new(inner: Rc<PipeInner>) -> Self {
        inner.writers.set(inner.writers.get() + 1);
        PipeWriter(inner)
    }

    pub fn inner(&self) -> &PipeInner {
        &self.0
    }
}

impl Clone for PipeWriter {
    fn clone(&self) -> Self {
        PipeWriter::new(Rc::clone(&self.0))
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        self.0.writers.set(self.0.writers.get().saturating_sub(1));
    }
}

/// 新建一对管道端点，返回 (读端, 写端)。
pub fn new_pipe() -> (FdKind, FdKind) {
    let inner = PipeInner::new();
    (
        FdKind::PipeRead(Rc::clone(&inner)),
        FdKind::PipeWrite(PipeWriter::new(inner)),
    )
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
        self.alloc_min(fd, 3)
    }

    /// 分配**不小于 `min`** 的最小可用 fd。`F_DUPFD` 要的正是这个语义。
    pub fn alloc_min(&mut self, fd: Fd, min: i32) -> i32 {
        let mut n = min.max(0);
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

    /// `fork` 时复制整张 fd 表。
    ///
    /// 不能 `derive(Clone)`：`File` 不是 `Clone`，只能 `try_clone`（宿主层面
    /// 的 `dup`，父子共享同一个文件偏移——正是 Linux `fork` 的语义）。
    /// 管道共享同一个 `Rc` 缓冲区，这是 fork 之后管道还能通的前提。
    /// `Dir` 的目录项缓存不复制：子进程重新枚举一次，`pos` 也归零，
    /// 因为缓存本身是我们的实现细节而不是 guest 可见状态。
    pub fn try_clone(&self) -> std::io::Result<FdTable> {
        let mut map = HashMap::with_capacity(self.map.len());
        for (&n, f) in &self.map {
            let kind = match &f.kind {
                FdKind::Stdin => FdKind::Stdin,
                FdKind::Stdout => FdKind::Stdout,
                FdKind::Stderr => FdKind::Stderr,
                FdKind::File(h) => FdKind::File(h.try_clone()?),
                FdKind::Dir { path, .. } => FdKind::Dir {
                    path: path.clone(),
                    entries: Vec::new(),
                    pos: 0,
                },
                FdKind::Dev(d) => FdKind::Dev(*d),
                FdKind::PipeRead(inner) => FdKind::PipeRead(Rc::clone(inner)),
                FdKind::PipeWrite(w) => FdKind::PipeWrite(w.clone()),
                FdKind::Closed => FdKind::Closed,
            };
            map.insert(
                n,
                Fd {
                    kind,
                    cloexec: f.cloexec,
                    flags: f.flags,
                },
            );
        }
        Ok(FdTable {
            map,
            next: self.next,
        })
    }
}

/// VFS：guest 路径 <-> 宿主路径。
#[derive(Clone)]
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

    /// 把 guest 路径规范化成 **guest 视角**的绝对路径。
    ///
    /// 纯字符串运算，**不碰宿主文件系统**：`..` 在这里就地消掉，这样
    /// `/a/../../etc/passwd` 会归约到 `/etc/passwd` 而不是逃到 prefix 之外。
    /// 根之上的 `..` 被吃掉（和 Linux 对 `/..` 的处理一致）。
    ///
    /// 只对**容器模式**（设了 prefix）有意义：结果一律以 `/` 开头，
    /// Windows 的盘符前缀会被丢掉。直通模式不要用它，见 `host_path`。
    pub fn normalize(&self, p: &str) -> PathBuf {
        self.normalize_checked(p).0
    }

    /// 同 `normalize`，另外报告这条路径**是否试图越过根**。
    ///
    /// 为什么要单独报告：`/..` 这类路径按内核语义会被"夹"到 `/`，于是
    /// 打开成功。夹住本身不会逃出 rootfs（这一点有 `host_path` 的测试保证），
    /// 但项目的安全审计（`tests/guest/t_sec_path.c`）要的是**更严**的一档：
    /// 越根的尝试要直接拒绝，而不是悄悄当成根。多一层拒绝没有代价——
    /// 正常程序里的 `..` 从不会弹出根之上。
    pub fn normalize_checked(&self, p: &str) -> (PathBuf, bool) {
        let raw = Path::new(p);
        let joined: PathBuf = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.cwd.join(raw)
        };
        let mut out: Vec<std::ffi::OsString> = Vec::new();
        let mut escaped = false;
        for c in joined.components() {
            match c {
                Component::RootDir | Component::Prefix(_) => out.clear(),
                Component::CurDir => {}
                Component::ParentDir => {
                    if out.pop().is_none() {
                        // 已经在根上还要往上：这就是越根尝试
                        escaped = true;
                    }
                }
                Component::Normal(s) => out.push(s.to_os_string()),
            }
        }
        let mut res = PathBuf::from("/");
        for s in out {
            res.push(s);
        }
        (res, escaped)
    }

    /// guest 路径 -> 宿主路径。两种模式语义不同：
    ///
    /// **容器模式**（设了 prefix）：先 `normalize` 消掉 `..`，再拼到 prefix 下。
    /// 因为 `..` 已经消完，结果必然落在 prefix 之内，不需要再
    /// `canonicalize`（那还会引入 TOCTOU）。
    ///
    /// **直通模式**（未设 prefix，guest `/` 就是宿主 `/`）：路径**原样**交给
    /// 宿主 OS 解析，只补相对路径的 cwd。这里绝不能走 `normalize`——
    /// `Path::components()` 在 Windows 上会把 `C:\x` 的盘符归成
    /// `Component::Prefix` 并被 `normalize` 丢掉，结果变成 `\x`，于是
    /// "找不到路径"。这个 bug 在 Windows CI 上实测踩到过，见下方回归测试。
    ///
    /// **已知缺口**：宿主侧的 symlink 不做防护——rootfs 里若存在指向外部的
    /// 符号链接，guest 能顺着它出去。与 blink 的限制相同，见 crate 文档。
    /// 把一条（可能是相对的）guest 路径变成 **guest 视角**的绝对路径字符串。
    ///
    /// 用于 `/proc/self/exe` 之类"要把路径回报给 guest"的场合：容器模式下
    /// 就是规范化后的 guest 绝对路径；直通模式下 guest 视角等于宿主视角，
    /// 所以直接用宿主绝对路径（不能走 `normalize`，Windows 盘符会被丢）。
    pub fn guest_abs(&self, guest: &str) -> String {
        match &self.prefix {
            None => self.host_path(guest).to_string_lossy().into_owned(),
            Some(_) => self.normalize(guest).to_string_lossy().into_owned(),
        }
    }

    pub fn host_path(&self, guest: &str) -> PathBuf {
        match &self.prefix {
            None => {
                let p = Path::new(guest);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    self.cwd.join(p)
                }
            }
            Some(pre) => {
                let norm = self.normalize(guest);
                let rel = norm.strip_prefix("/").unwrap_or(&norm);
                pre.join(rel)
            }
        }
    }

    /// 带越根检查的 guest -> 宿主翻译。
    ///
    /// 容器模式下越根尝试返回 `None`，调用方据此回 `EACCES`；
    /// 直通模式没有"根"可越，一律放行。
    pub fn host_path_confined(&self, guest: &str) -> Option<PathBuf> {
        if self.prefix.is_some() && self.normalize_checked(guest).1 {
            return None;
        }
        Some(self.host_path(guest))
    }

    /// `chdir` 的目标：容器模式记 guest 视角路径，直通模式记宿主路径。
    /// 两者都必须和 `host_path` 的同模式解释保持一致。
    pub fn cwd_for(&self, guest: &str) -> PathBuf {
        match &self.prefix {
            None => self.host_path(guest),
            Some(_) => self.normalize(guest),
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
        assert_eq!(
            v.normalize("/../../etc/passwd"),
            PathBuf::from("/etc/passwd")
        );
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

    /// 回归：直通模式必须把宿主绝对路径**原样**交给 OS。
    ///
    /// 曾经这里统一走 `normalize`，而 `Path::components()` 在 Windows 上把盘符
    /// 归成 `Component::Prefix`、被 `normalize` 的 `out.clear()` 丢掉，于是
    /// `C:\Users\foo` 变成 `\Users\foo`，guest 一律"找不到路径"。
    /// Windows CI 上 10 个端到端用例因此全红——Linux 上则完全看不出来。
    #[test]
    fn passthrough_keeps_host_absolute_path_intact() {
        let v = vfs(None);
        let native_abs = if cfg!(windows) {
            r"C:\Users\runner\tmp\prog"
        } else {
            "/home/runner/tmp/prog"
        };
        assert_eq!(v.host_path(native_abs), PathBuf::from(native_abs));
        // 与平台无关的不变量：直通模式不得丢掉任何前导组件
        let got = v.host_path(native_abs);
        assert!(
            got.to_string_lossy().contains("runner"),
            "路径组件被吃掉了：{got:?}"
        );
        #[cfg(windows)]
        assert!(
            got.to_string_lossy().starts_with("C:"),
            "盘符必须保留：{got:?}"
        );
    }

    /// 直通模式下相对路径按 cwd 解析，cwd 本身可能是宿主风格路径。
    #[test]
    fn passthrough_resolves_relative_against_host_cwd() {
        let mut v = vfs(None);
        v.cwd = if cfg!(windows) {
            PathBuf::from(r"C:\work")
        } else {
            PathBuf::from("/work")
        };
        assert_eq!(v.host_path("sub/prog"), v.cwd.join("sub/prog"));
    }

    /// 容器模式的 cwd 是 guest 视角路径；直通模式的 cwd 是宿主路径。
    #[test]
    fn cwd_for_matches_the_mode() {
        let v = vfs(Some("/srv/rootfs"));
        assert_eq!(v.cwd_for("/a/b/../c"), PathBuf::from("/a/c"));

        let v = vfs(None);
        let abs = if cfg!(windows) { r"C:\a\b" } else { "/a/b" };
        assert_eq!(v.cwd_for(abs), PathBuf::from(abs));
    }

    /// 越根尝试必须被**拒绝**，而不是夹到根。
    ///
    /// 这是项目安全审计（`tests/guest/t_sec_path.c`）要求的更严一档：
    /// 内核语义会把 `/..` 夹成 `/` 于是 open 成功；夹住本身不会逃出 rootfs，
    /// 但审计要求越根的尝试直接失败。正常程序里的 `..` 从不弹出根之上，
    /// 所以这层拒绝没有误伤。
    #[test]
    fn above_root_attempts_are_rejected_not_clamped() {
        let v = vfs(Some("/srv/rootfs"));
        for probe in ["/..", "/../../..", "/tmp/../../..", "../../../.."] {
            assert!(
                v.host_path_confined(probe).is_none(),
                "{probe} 应被拒绝，而不是夹到根"
            );
            assert!(v.normalize_checked(probe).1, "{probe} 应被判定为越根");
        }
        // 合法的 `..`（不弹出根）必须照常放行
        for ok in ["/usr/lib/../bin/sh", "/a/b/../c", "/."] {
            assert!(v.host_path_confined(ok).is_some(), "{ok} 被误拒");
            assert!(!v.normalize_checked(ok).1, "{ok} 被误判越根");
        }
        // 即使被拒绝，夹住的结果本身也仍在 prefix 内（双保险）
        assert!(v.host_path("/../../..").starts_with("/srv/rootfs"));
    }

    /// 直通模式没有"根"可越，不做这层拒绝——否则 `wbox-linux /bin/cat ..`
    /// 这类正常用法会莫名 EACCES。
    #[test]
    fn passthrough_does_not_apply_above_root_rejection() {
        let v = vfs(None);
        assert!(v.host_path_confined("/..").is_some());
    }

    /// 容器模式**不受**直通改动影响：盘符风格的输入仍被当作 guest 路径收进
    /// prefix 内，绝不能因为"原样透传"而逃出去。
    #[test]
    fn prefix_mode_still_confines_windows_style_input() {
        let v = vfs(Some("/srv/rootfs"));
        for probe in [
            r"C:\Windows\System32",
            r"..\..\Windows",
            "/../../etc/shadow",
        ] {
            let got = v.host_path(probe);
            assert!(
                got.starts_with("/srv/rootfs"),
                "{probe} 逃出了 prefix：{got:?}"
            );
        }
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
        let a = t.alloc(Fd {
            kind: FdKind::Closed,
            cloexec: false,
            flags: 0,
        });
        let b = t.alloc(Fd {
            kind: FdKind::Closed,
            cloexec: false,
            flags: 0,
        });
        assert_eq!((a, b), (3, 4));
        t.remove(3);
        // Linux 保证下一个 open 拿到最小空号
        let c = t.alloc(Fd {
            kind: FdKind::Closed,
            cloexec: false,
            flags: 0,
        });
        assert_eq!(c, 3);
    }

    #[test]
    fn close_on_exec_drops_only_cloexec_fds() {
        let mut t = FdTable::new();
        let keep = t.alloc(Fd {
            kind: FdKind::Closed,
            cloexec: false,
            flags: 0,
        });
        let drop = t.alloc(Fd {
            kind: FdKind::Closed,
            cloexec: true,
            flags: 0,
        });
        t.close_on_exec();
        assert!(t.contains(keep));
        assert!(!t.contains(drop));
        assert!(t.contains(0), "标准流不带 CLOEXEC");
    }
}
