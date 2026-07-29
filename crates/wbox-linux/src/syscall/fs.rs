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

/// `RLIMIT_NOFILE` 的默认软上限（与 `getrlimit` 报的一致）。
pub const DEFAULT_NOFILE: u64 = 1024;
/// 硬上限。
pub const MAX_NOFILE: u64 = 4096;

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
    PipeRead(PipeReader),
    /// 匿名管道的写端。持有 `PipeWriter` 而不是裸 `Rc`，这样"还有几个写端
    /// 开着"是自动记账的，见 `PipeWriter`。
    PipeWrite(PipeWriter),
    /// 合成的字符设备（`/dev/null` 等），见 `DevKind`。
    Dev(DevKind),
    /// 套接字。见 `syscall::net`。
    Socket(Rc<crate::syscall::net::Socket>),
    /// `eventfd`：一个 64 位计数器。见 [`EventFd`]。
    Event(Rc<EventFd>),
    /// `timerfd`：到期计数器。见 [`TimerFd`]。
    Timer(Rc<TimerFd>),
    /// epoll 实例。
    Epoll(Rc<crate::syscall::net::Epoll>),
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
    /// 还开着的**读端**个数。写端据此报 `EPIPE` / `POLLERR`。
    readers: Cell<usize>,
    /// 缓冲区容量（`F_GETPIPE_SZ` / `F_SETPIPE_SZ`）。
    ///
    /// # 只对非阻塞写强制
    ///
    /// 真内核里写满就阻塞，等读端取走。可这个模拟器是**单线程 + 快照式
    /// fork**：`a | b` 里的 a 必须先跑完，b 才开始读。真按容量卡住阻塞写，
    /// 一条输出超过 64 KiB 的管道会当场死锁——而那是极常见的用法。
    ///
    /// 所以容量只在 `O_NONBLOCK` 写上生效（那时正确答案是 `EAGAIN`，不是
    /// 阻塞），阻塞写允许超出容量。这是被执行模型逼出来的偏差，如实记在
    /// 这里，不假装是完整语义。
    capacity: Cell<usize>,
    /// 写入代次：每成功写一次 +1。
    ///
    /// 边沿触发（`EPOLLET`）要报的是**"又来新数据了"这个瞬间**，而不是
    /// "现在有数据"。单线程模拟器没有内核那样的写入钩子，只能在 `epoll_wait`
    /// 时回看，所以需要一个"自上次上报以来有没有新写入"的证据——就是它。
    /// 少了这一位，"读空 → 再写入"这条最常见的 ET 序列会被当成同一次就绪
    /// 而漏报（`t_net_epoll` 的 epoll/et-pipe 正是这么抓到的）。
    epoch: Cell<u64>,
}

/// 管道默认容量。与 Linux 一致（`/proc/sys/fs/pipe-max-size` 的默认页数）。
pub const PIPE_DEFAULT_CAPACITY: usize = 64 * 1024;
/// `F_SETPIPE_SZ` 的下界：内核会把小于一页的请求抬到一页。
pub const PIPE_MIN_CAPACITY: usize = 4096;

impl PipeInner {
    fn new() -> Rc<Self> {
        Rc::new(PipeInner {
            data: RefCell::new(VecDeque::new()),
            writers: Cell::new(0),
            readers: Cell::new(0),
            capacity: Cell::new(PIPE_DEFAULT_CAPACITY),
            epoch: Cell::new(0),
        })
    }

    /// 写端是否已全部关闭（读端据此报 EOF）。
    pub fn writers_closed(&self) -> bool {
        self.writers.get() == 0
    }

    /// 读端是否已全部关闭（写端据此报 `EPIPE`／`POLLERR`）。
    pub fn readers_closed(&self) -> bool {
        self.readers.get() == 0
    }

    pub fn capacity(&self) -> usize {
        self.capacity.get()
    }

    /// 设置容量，返回内核实际采用的值（不小于一页）。
    pub fn set_capacity(&self, want: usize) -> usize {
        let v = want.max(PIPE_MIN_CAPACITY);
        self.capacity.set(v);
        v
    }

    pub fn epoch(&self) -> u64 {
        self.epoch.get()
    }

    /// 记一次写入（见 `epoch` 字段的说明）。
    pub fn bump_epoch(&self) {
        self.epoch.set(self.epoch.get().wrapping_add(1));
    }

    /// 还能无阻塞地写进去多少字节。
    pub fn space(&self) -> usize {
        self.capacity.get().saturating_sub(self.data.borrow().len())
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

    /// 同 [`PipeReader::share`]：不计入写端计数的共享句柄。
    pub fn share(&self) -> Rc<PipeInner> {
        Rc::clone(&self.0)
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

/// `eventfd(2)`：一个 64 位计数器，读取即清零（或信号量模式下减一）。
///
/// 与管道不同，它**没有缓冲区**——所以 `read`/`write` 一次必须正好 8 字节，
/// 少一个字节都是 `EINVAL`。这条看着琐碎，却是 guest 侧库判断"这是不是真的
/// eventfd"的常用手段。
pub struct EventFd {
    pub counter: Cell<u64>,
    /// `EFD_SEMAPHORE`：每次读只取走 1，而不是取走全部。
    pub semaphore: bool,
    /// 写入代次，供 epoll 的边沿触发判断（语义同 `PipeInner::epoch`）。
    pub epoch: Cell<u64>,
}

/// 计数器上界：`write` 使计数达到它就是溢出（Linux 用 `0xffff_ffff_ffff_ffff`
/// 作为非法写入值，计数最大值是它减一）。
pub const EVENTFD_MAX: u64 = u64::MAX - 1;

impl EventFd {
    pub fn new(init: u64, semaphore: bool) -> Rc<EventFd> {
        Rc::new(EventFd {
            counter: Cell::new(init),
            semaphore,
            epoch: Cell::new(0),
        })
    }

    /// 读一次。`None` 表示计数为 0（调用方按阻塞与否报 `EAGAIN`）。
    pub fn take(&self) -> Option<u64> {
        let c = self.counter.get();
        if c == 0 {
            return None;
        }
        if self.semaphore {
            self.counter.set(c - 1);
            Some(1)
        } else {
            self.counter.set(0);
            Some(c)
        }
    }

    /// 写一次。返回 `false` = 会溢出（调用方报 `EAGAIN`）。
    pub fn add(&self, v: u64) -> bool {
        let c = self.counter.get();
        let Some(n) = c.checked_add(v) else {
            return false;
        };
        if n > EVENTFD_MAX {
            return false;
        }
        self.counter.set(n);
        self.epoch.set(self.epoch.get().wrapping_add(1));
        true
    }
}

/// `timerfd(2)`：一个按时钟到期的计数器。
///
/// # 到期是**惰性算出来的**，没有后台线程
///
/// 单线程模拟器里没有定时器中断可用，也不该为每个 timerfd 起一个线程。
/// 所以只记下"下一次到期的绝对时刻"和周期，在 `read`/`poll`/`epoll_wait`
/// 真正问起来的时候，拿当前时间回算这中间跨过了几个周期。
///
/// 这与内核的可观测行为一致：guest 只能通过读或轮询看到到期，看不到"内核
/// 在哪一刻记的账"。周期定时器连续跨过多个周期时一次性返回累计次数，也正是
/// Linux 的语义。
pub struct TimerFd {
    /// 下一次到期的绝对纳秒时刻；0 = 未武装。
    pub deadline_ns: Cell<u64>,
    /// 周期；0 = 一次性。
    pub interval_ns: Cell<u64>,
    /// 已到期但还没被读走的次数。
    pub expirations: Cell<u64>,
    /// 写入代次，供 epoll 边沿触发（语义同 `PipeInner::epoch`）。
    pub epoch: Cell<u64>,
}

impl TimerFd {
    pub fn new() -> Rc<TimerFd> {
        Rc::new(TimerFd {
            deadline_ns: Cell::new(0),
            interval_ns: Cell::new(0),
            expirations: Cell::new(0),
            epoch: Cell::new(0),
        })
    }

    /// 把到当前时刻为止的到期次数结算进 `expirations`。
    pub fn settle(&self, now_ns: u64) {
        let dl = self.deadline_ns.get();
        if dl == 0 || now_ns < dl {
            return;
        }
        let iv = self.interval_ns.get();
        let elapsed = now_ns - dl;
        let n = if let Some(extra) = elapsed.checked_div(iv) {
            // 跨过了几个周期就记几次，并把下一次到期推到当前时刻之后。
            self.deadline_ns.set(dl + (extra + 1) * iv);
            extra + 1
        } else {
            self.deadline_ns.set(0); // 一次性：打完收工
            1
        };
        self.expirations
            .set(self.expirations.get().saturating_add(n));
        self.epoch.set(self.epoch.get().wrapping_add(1));
    }

    /// 取走并清零。0 表示还没到期。
    pub fn take(&self, now_ns: u64) -> u64 {
        self.settle(now_ns);
        let n = self.expirations.get();
        self.expirations.set(0);
        n
    }

    /// 距下次到期还有多久（`timerfd_gettime` 的 `it_value`）。
    pub fn remaining(&self, now_ns: u64) -> u64 {
        let dl = self.deadline_ns.get();
        if dl == 0 {
            return 0;
        }
        dl.saturating_sub(now_ns)
    }
}

/// 读端的持有凭证：构造 +1、`Drop` -1。理由同 [`PipeWriter`]——
/// 写端要靠"还有没有读端"来决定 `EPIPE`／`POLLERR`，手工记账必漏。
pub struct PipeReader(Rc<PipeInner>);

impl PipeReader {
    fn new(inner: Rc<PipeInner>) -> Self {
        inner.readers.set(inner.readers.get() + 1);
        PipeReader(inner)
    }

    pub fn inner(&self) -> &PipeInner {
        &self.0
    }

    /// 拿一份**不计入读端计数**的共享句柄。
    ///
    /// epoll 要长期持有被监视对象，但它不是"一个读端"——若走 `clone()`，
    /// 读端计数会一直不归零，写端就永远等不到 `EPIPE`/`POLLERR`。
    pub fn share(&self) -> Rc<PipeInner> {
        Rc::clone(&self.0)
    }
}

impl Clone for PipeReader {
    fn clone(&self) -> Self {
        PipeReader::new(Rc::clone(&self.0))
    }
}

impl Drop for PipeReader {
    fn drop(&mut self) {
        self.0.readers.set(self.0.readers.get().saturating_sub(1));
    }
}

/// 新建一对管道端点，返回 (读端, 写端)。
pub fn new_pipe() -> (FdKind, FdKind) {
    let inner = PipeInner::new();
    (
        FdKind::PipeRead(PipeReader::new(Rc::clone(&inner))),
        FdKind::PipeWrite(PipeWriter::new(inner)),
    )
}

pub struct Fd {
    pub kind: FdKind,
    /// `O_CLOEXEC`：`execve` 时要关掉。
    ///
    /// **这一项是"文件描述符"自己的**，不随 `dup` 传递——POSIX 明确规定
    /// `dup` 出来的新 fd 不继承 `FD_CLOEXEC`。
    pub cloexec: bool,
    /// 状态标志（`O_APPEND`／`O_NONBLOCK` 等），`fcntl(F_GETFL)` 要回它。
    ///
    /// **这一项属于"打开文件描述"（open file description），不属于 fd。**
    /// `dup`、`dup2`、`F_DUPFD`、`fork` 产生的别名共享同一份：在任意一个
    /// 别名上 `F_SETFL O_NONBLOCK`，其余别名 `F_GETFL` 必须立刻看得到。
    /// 早先每个 `Fd` 各存一份 `i32`，于是 `dup` 之后两边的标志各走各的——
    /// `t_fd_open` 的 dup/shared-append-status 与 `t_fd_rw` 的
    /// pipe/dup-shares-nonblock 抓的就是这个。
    status: Rc<Cell<i32>>,
}

impl Fd {
    pub fn new(kind: FdKind, cloexec: bool, flags: i32) -> Self {
        Fd {
            kind,
            cloexec,
            status: Rc::new(Cell::new(flags)),
        }
    }

    pub fn flags(&self) -> i32 {
        self.status.get()
    }

    pub fn set_flags(&self, v: i32) {
        self.status.set(v);
    }

    /// 复制出一个**共享同一份状态标志**的句柄（`dup` 家族与 `fork` 用）。
    pub fn alias(&self, kind: FdKind, cloexec: bool) -> Self {
        Fd {
            kind,
            cloexec,
            status: Rc::clone(&self.status),
        }
    }
}

pub struct FdTable {
    map: HashMap<i32, Fd>,
    next: i32,
    /// `RLIMIT_NOFILE` 的**软**上限：可分配的 fd 号必须严格小于它。
    ///
    /// 放在 fd 表里而不是别处，是因为"分配 fd"这件事只有这里做得到原子拒绝。
    /// 分配点散着好几处，若让每个调用方自己先查一遍上限，漏掉的那处就是
    /// "限额说了不算"——`t_negative` 的 rlimit-nofile-* 专盯这个。
    nofile: u64,
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
            map.insert(n, Fd::new(k, false, 0));
        }
        FdTable {
            map,
            next: 3,
            nofile: DEFAULT_NOFILE,
        }
    }

    /// 分配最小可用 fd（Linux 保证的语义：总是最小的空号）。
    ///
    /// 下界是 **0 而不是 3**。平时 0/1/2 都占着，自然从 3 起分；但 guest
    /// 主动 `close(0)` 之后，下一个 `open`/`pipe` 就该拿到 0——把 stdin
    /// 重定向成管道读端正是这么写的（`close(0); pipe(p);`）。早先写死 3，
    /// 那个惯用法就静默失效（`t_fd_open` 的 pipe/fork-child-lowest-fds）。
    pub fn alloc(&mut self, fd: Fd) -> Option<i32> {
        self.alloc_min(fd, 0)
    }

    /// 分配**不小于 `min`** 的最小可用 fd。`F_DUPFD` 要的正是这个语义。
    ///
    /// 超过 `RLIMIT_NOFILE` 软上限时返回 `None`（调用方报 `EMFILE`）。
    pub fn alloc_min(&mut self, fd: Fd, min: i32) -> Option<i32> {
        let mut n = min.max(0);
        while self.map.contains_key(&n) {
            n += 1;
        }
        if (n as u64) >= self.nofile {
            return None;
        }
        self.map.insert(n, fd);
        self.next = n + 1;
        Some(n)
    }

    pub fn nofile(&self) -> u64 {
        self.nofile
    }

    pub fn set_nofile(&mut self, v: u64) {
        self.nofile = v;
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
                FdKind::PipeRead(r) => FdKind::PipeRead(r.clone()),
                // 套接字与 epoll 实例按引用复制：fork 出来的子进程与父进程
                // 共享同一个对象，这正是 Linux 的语义（描述被继承而不是复制）。
                FdKind::Socket(s) => FdKind::Socket(Rc::clone(s)),
                FdKind::Event(e) => FdKind::Event(Rc::clone(e)),
                FdKind::Timer(t) => FdKind::Timer(Rc::clone(t)),
                FdKind::Epoll(e) => FdKind::Epoll(Rc::clone(e)),
                FdKind::PipeWrite(w) => FdKind::PipeWrite(w.clone()),
                FdKind::Closed => FdKind::Closed,
            };
            // fork 出来的 fd 与父进程**共享同一个打开文件描述**：偏移、
            // O_APPEND、O_NONBLOCK 都是共享的（`File::try_clone` 走宿主
            // `dup`，偏移天然共享；状态标志靠这里共享同一个 `Rc`）。
            map.insert(n, f.alias(kind, f.cloexec));
        }
        Ok(FdTable {
            map,
            nofile: self.nofile,
            next: self.next,
        })
    }
}

/// 一条 `mount(2)` 记录。
///
/// # 为什么是"路径改写"而不是真挂载
///
/// 引擎跑在用户态，没有挂载命名空间可用（Windows 宿主上更没有）。所以
/// `mount` 落成一条**路径改写规则**：guest 访问挂载点之下的路径时，前缀被
/// 换成源目录。这对 guest 是不可分辨的——它只能通过路径看到结果。
///
/// 换来的好处是 `MS_RDONLY` 可以真的生效：只读标记跟着这条规则走，写类
/// 系统调用查一次就知道该不该报 `EROFS`。
#[derive(Clone)]
pub struct Mount {
    /// guest 侧挂载点，已拆成规范化的段。
    pub target: Vec<std::ffi::OsString>,
    /// 宿主侧的源目录。
    pub source: PathBuf,
    pub readonly: bool,
}

/// VFS：guest 路径 <-> 宿主路径。
#[derive(Clone)]
pub struct Vfs {
    /// guest `/` 对应的宿主目录。`None` 表示直通宿主根（无 rootfs 隔离）。
    pub prefix: Option<PathBuf>,
    /// guest 的当前工作目录（**guest 视角**的绝对路径）。
    pub cwd: PathBuf,
    /// 已建立的挂载，后加的优先（最长前缀匹配）。
    pub mounts: Vec<Mount>,
}

/// 受限路径解析的失败原因。
///
/// 两者**必须分开**：guest 程序按 POSIX 语义分辨 `EACCES` 与 `ELOOP`，
/// 合并成一档会让 `open` 在链接成环时返回"权限不足"，与真实内核不符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveErr {
    /// 试图越过 rootfs 根（`/..`、`../../..` 之类）。
    Escaped,
    /// 符号链接成环，展开深度超过 [`MAX_SYMLINK_DEPTH`]。
    Loop,
}

impl ResolveErr {
    /// 对应的 Linux 负 errno，直接可作为 syscall 返回值。
    pub fn errno(self) -> i64 {
        match self {
            // 13 = EACCES，40 = ELOOP。这里写字面量而不是引用 syscall::mod
            // 的常量，是为了不让 fs 反向依赖上层模块。
            ResolveErr::Escaped => -13,
            ResolveErr::Loop => -40,
        }
    }
}

/// 符号链接展开的深度上限，等同内核的 `MAXSYMLINKS`（40）。
/// 超过就认为成环——真实 rootfs 里正常链接远到不了这个数。
const MAX_SYMLINK_DEPTH: u32 = 40;

/// 这条路径在 **guest 眼里**是不是绝对路径。
///
/// 不能只用 `Path::is_absolute()`：那是**宿主**语义，Windows 上
/// `"/etc/passwd"` 会被判成相对路径（它要 `C:\` 那样的盘符）。而 guest 是
/// Linux，`/` 开头就是绝对。判错的后果是绝对符号链接不按 rootfs 根解析，
/// 那正是要防的越狱路径。
fn guest_is_absolute(p: &Path) -> bool {
    p.to_string_lossy().starts_with('/') || p.is_absolute()
}

/// 把路径拆成待解析的段，压进队列。
///
/// 根/盘符前缀丢弃（起点由调用方决定），`.` 跳过，`..` 原样保留成一个段
/// ——它要在**解析栈**上生效，不能在这里就地消掉（就地消掉正是旧实现挡不住
/// 符号链接的原因）。
fn push_segments(p: &Path, out: &mut std::collections::VecDeque<std::ffi::OsString>) {
    for c in p.components() {
        match c {
            Component::RootDir | Component::Prefix(_) => {}
            Component::CurDir => {}
            Component::ParentDir => out.push_back(std::ffi::OsString::from("..")),
            Component::Normal(s) => out.push_back(s.to_os_string()),
        }
    }
}

/// 把已解析的组件栈拼回 prefix 之下。
fn join_under(pre: &Path, stack: &[std::ffi::OsString]) -> PathBuf {
    let mut out = pre.to_path_buf();
    for s in stack {
        out.push(s);
    }
    out
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
        Vfs {
            prefix,
            cwd,
            mounts: Vec::new(),
        }
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

    /// guest 路径 -> 宿主路径，**末段符号链接也跟随**。
    ///
    /// 这是默认入口，`open`/`stat`/`execve` 这类"会跟随"的 syscall 用它。
    /// 不跟随末段的那一小撮（`lstat`/`readlink`/`unlink`/`symlink`/`rename`
    /// /`link`）用 [`Vfs::host_path_nofollow`]。
    ///
    /// **默认选跟随，是在选失败方向**：漏标一个"其实不该跟随"的调用点，
    /// 表现是功能不对（`lstat` 看到的是目标而不是链接本身），测试会抓到；
    /// 反过来把默认设成不跟随，漏标一个"其实会跟随"的调用点，表现是
    /// **静默的越狱**——没有任何东西会提醒你。
    pub fn host_path(&self, guest: &str) -> PathBuf {
        self.translate(guest, true).0
    }

    /// 该 guest 路径是否落在**只读挂载**之下（写类系统调用据此报 `EROFS`）。
    pub fn is_readonly(&self, guest: &str) -> bool {
        self.translate(guest, true).2
    }

    /// 把已解析的 guest 段栈映射到宿主路径，按**最长前缀**匹配挂载表。
    ///
    /// 后加的挂载覆盖先加的同前缀者（Linux 的挂载栈语义），所以相同长度时
    /// 取靠后的那条。
    fn map_mount(&self, pre: &Path, stack: &[std::ffi::OsString]) -> (PathBuf, bool) {
        let mut best: Option<&Mount> = None;
        for mnt in &self.mounts {
            if stack.len() < mnt.target.len() || stack[..mnt.target.len()] != mnt.target[..] {
                continue;
            }
            if best.is_none_or(|b| mnt.target.len() >= b.target.len()) {
                best = Some(mnt);
            }
        }
        match best {
            Some(mn) => (
                join_under(&mn.source, &stack[mn.target.len()..]),
                mn.readonly,
            ),
            None => (join_under(pre, stack), false),
        }
    }

    /// 把 guest 绝对路径拆成规范化的段（`mount` 记挂载点时用）。
    pub fn guest_segments(&self, guest: &str) -> Vec<std::ffi::OsString> {
        let abs = self.normalize(guest);
        let mut q = std::collections::VecDeque::new();
        push_segments(&abs, &mut q);
        let mut stack: Vec<std::ffi::OsString> = Vec::new();
        for seg in q {
            if seg == ".." {
                stack.pop();
            } else {
                stack.push(seg);
            }
        }
        stack
    }

    /// 同 [`Vfs::host_path`]，但**不跟随末段**符号链接。
    ///
    /// 中间各段照样受限解析——`/link/to/outside/x` 里的 `link` 仍会被夹在
    /// rootfs 内。只有最后一段保持原样，交给调用方的 `symlink_metadata` /
    /// `read_link` / `remove_file` 去处理（它们本来就不跟随）。
    pub fn host_path_nofollow(&self, guest: &str) -> PathBuf {
        self.translate(guest, false).0
    }

    /// 路径翻译的唯一实现。返回 `(宿主路径, 是否有越根尝试)`。
    ///
    /// # 容器模式：用户态的 `RESOLVE_IN_ROOT`
    ///
    /// 早先这里只做**词法**规范化（把 `..` 就地消掉再拼到 prefix 下）。
    /// 那挡得住 `../../etc/passwd`，但**完全挡不住符号链接**：rootfs 里
    /// 一个 `/evil -> /` 的链接，guest 打开 `/evil/etc/shadow` 时词法上
    /// 一路合法，内核在**宿主**上跟着链接走，直接读到宿主的
    /// `/etc/shadow`。镜像是外部输入，造这么一个链接零成本。
    ///
    /// 现在改成逐段解析：每走一段就看它是不是符号链接，是就把链接目标
    /// 展开后**重新从 rootfs 根开始**解析。关键在于 `..` 与绝对目标都作用在
    /// 这个"已解析栈"上，而栈**空了就到根**——所以**结构上不可能**指到
    /// prefix 之外，不是靠事后检查兜。这与内核 `openat2(RESOLVE_IN_ROOT)`
    /// 的语义一致。
    ///
    /// # 为什么不直接用 `openat2(RESOLVE_IN_ROOT)`
    ///
    /// 它只在 Linux 5.6+ 有，而这个 crate **也要在 Windows 宿主上跑**
    /// （PRD §2.4 的 Q2 象限就是 Windows 上跑 Linux 镜像）。用它就得写两套
    /// 路径解析，而"两套实现"在本仓是踩过的坑。这里一套可移植实现两边共用。
    ///
    /// 代价诚实记下来：**每段一次 `symlink_metadata`**，比原来的纯字符串
    /// 运算慢；以及 check-then-use 的 TOCTOU 窗口——`openat2` 是原子的，
    /// 这里不是。当前进程模型下窗口很小（快照式 fork，父子不并发跑），
    /// 但它确实存在，见 crate 文档。
    fn translate(&self, guest: &str, follow_final: bool) -> (PathBuf, Option<ResolveErr>, bool) {
        let Some(pre) = self.prefix.as_ref() else {
            // 直通模式：guest / 就是宿主 /，没有"根"可越，路径原样交给宿主
            // 解析。这里绝不能走组件分解——Windows 上盘符会被丢掉。
            let p = Path::new(guest);
            let host = if p.is_absolute() {
                p.to_path_buf()
            } else {
                self.cwd.join(p)
            };
            return (host, None, false);
        };

        let mut escaped = None;
        // 已解析的 guest 组件。栈空 == 位于 rootfs 根。
        let mut stack: Vec<std::ffi::OsString> = Vec::new();
        let mut pending: std::collections::VecDeque<std::ffi::OsString> =
            std::collections::VecDeque::new();

        let start = if guest_is_absolute(Path::new(guest)) {
            PathBuf::from(guest)
        } else {
            self.cwd.join(guest)
        };
        push_segments(&start, &mut pending);

        let mut links = 0u32;
        while let Some(seg) = pending.pop_front() {
            if seg == ".." {
                if stack.pop().is_none() {
                    // 已经在根上还要往上：记为越根尝试，位置夹在根
                    // （与内核对 `/..` 的处理一致）。
                    escaped = Some(ResolveErr::Escaped);
                }
                continue;
            }
            stack.push(seg);

            // 末段是否展开由 follow_final 决定；中间段一律展开。
            if pending.is_empty() && !follow_final {
                break;
            }
            let here = join_under(pre, &stack);
            let Ok(md) = std::fs::symlink_metadata(&here) else {
                // 不存在（创建新文件是常态）或没权限看：不再往下解析。
                // 剩余组件原样拼上——它们同样不可能逃出去，因为 `..`
                // 仍然作用在这个栈上。
                continue;
            };
            if !md.file_type().is_symlink() {
                continue;
            }
            links += 1;
            if links > MAX_SYMLINK_DEPTH {
                // 链接成环。**必须报 ELOOP 而不是 EACCES**：guest 侧的
                // 程序按 POSIX 语义分辨这两者，`t_path` 的 symlink/loop-ELOOP
                // 就直接断言了这一点。早先图省事把它并进"越根"一档，
                // 表现是 `open` 返回 13 而不是 40，测试当场抓到。
                let (p, ro) = self.map_mount(pre, &stack);
                return (p, Some(ResolveErr::Loop), ro);
            }
            let Ok(target) = std::fs::read_link(&here) else {
                continue;
            };
            stack.pop();
            if guest_is_absolute(&target) {
                // **guest 里的绝对链接以 rootfs 为根**，不是宿主根。
                // 清栈就等于"回到 rootfs 根重新解析"。
                stack.clear();
            }
            let mut expanded = std::collections::VecDeque::new();
            push_segments(&target, &mut expanded);
            while let Some(x) = expanded.pop_back() {
                pending.push_front(x);
            }
        }

        let (p, ro) = self.map_mount(pre, &stack);
        (p, escaped, ro)
    }

    /// 带越根检查的 guest -> 宿主翻译。
    ///
    /// 容器模式下越根尝试返回 `None`，调用方据此回 `EACCES`；
    /// 直通模式没有"根"可越，一律放行。
    pub fn host_path_confined(&self, guest: &str) -> Result<PathBuf, ResolveErr> {
        match self.translate(guest, true) {
            (host, None, _) => Ok(host),
            (_, Some(e), _) => Err(e),
        }
    }

    /// 同 [`Vfs::host_path_confined`]，但不跟随末段符号链接。
    pub fn host_path_confined_nofollow(&self, guest: &str) -> Result<PathBuf, ResolveErr> {
        match self.translate(guest, false) {
            (host, None, _) => Ok(host),
            (_, Some(e), _) => Err(e),
        }
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
            mounts: Vec::new(),
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

    /// 造一个真实的 rootfs，里面放各种指向宿主的符号链接，逐条断言
    /// **解析结果仍在 rootfs 内**。
    ///
    /// 这是 PRD §4.9 那条"已知缺口"的回归用例。旧实现只做词法规范化，
    /// 下面每一条都能逃出去。
    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape_the_rootfs() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("wbox-vfs-esc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("rootfs");
        std::fs::create_dir_all(root.join("etc")).unwrap();
        std::fs::create_dir_all(base.join("outside")).unwrap();
        std::fs::write(base.join("outside/secret.txt"), b"HOST SECRET").unwrap();
        std::fs::write(root.join("etc/passwd"), b"guest passwd").unwrap();

        // 四种典型的逃逸链接
        symlink("/", root.join("slash")).unwrap(); // 指向 guest 根
        symlink(&base, root.join("up")).unwrap(); // 绝对指向宿主
        symlink("../../outside", root.join("rel")).unwrap(); // 相对往上爬
        std::fs::create_dir_all(root.join("d")).unwrap();
        symlink("../../../../..", root.join("d/climb")).unwrap(); // 深度往上爬

        let v = vfs(Some(root.to_str().unwrap()));

        for probe in [
            "/up/outside/secret.txt",
            "/rel/secret.txt",
            "/d/climb/outside/secret.txt",
            "/slash/../../outside/secret.txt",
            "/up/../outside/secret.txt",
        ] {
            let host = v.host_path(probe);
            assert!(
                host.starts_with(&root),
                "{probe} 解析到了 rootfs 之外：{}",
                host.display()
            );
            // 更硬的判据：真去读，绝不能读到宿主那份内容。
            if let Ok(data) = std::fs::read(&host) {
                assert_ne!(
                    data, b"HOST SECRET",
                    "{probe} 读到了宿主文件内容（越狱成功）"
                );
            }
        }

        // 反向：正常路径与 rootfs 内部链接仍要能用——只测"挡得住"会让一个
        // 恒返回根目录的实现也变绿。
        symlink("/etc/passwd", root.join("alias")).unwrap();
        assert_eq!(
            std::fs::read(v.host_path("/alias")).unwrap(),
            b"guest passwd"
        );
        assert_eq!(
            std::fs::read(v.host_path("/etc/passwd")).unwrap(),
            b"guest passwd"
        );
        // `/slash` 指向 guest 根，`/slash/etc/passwd` 应当正常解析到 rootfs 内
        assert_eq!(
            std::fs::read(v.host_path("/slash/etc/passwd")).unwrap(),
            b"guest passwd"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    /// 末段链接：`host_path` 跟随，`host_path_nofollow` 不跟随。
    ///
    /// 两者都必须夹在 rootfs 内——区别只在"看到的是链接还是目标"。
    #[cfg(unix)]
    #[test]
    fn final_component_follow_semantics() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("wbox-vfs-fin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("rootfs");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(base.join("outside")).unwrap();
        std::fs::write(base.join("outside/secret.txt"), b"HOST SECRET").unwrap();
        std::fs::write(root.join("real.txt"), b"guest data").unwrap();
        symlink("real.txt", root.join("link")).unwrap();
        // 末段直接指向宿主
        symlink(base.join("outside/secret.txt"), root.join("evil")).unwrap();

        let v = vfs(Some(root.to_str().unwrap()));

        // 跟随：解到 real.txt 本身
        assert_eq!(v.host_path("/link"), root.join("real.txt"));
        // 不跟随：停在链接上（lstat/readlink 要的就是这个）
        assert_eq!(v.host_path_nofollow("/link"), root.join("link"));

        // **末段指向宿主的链接，跟随时也不能逃出去**
        let host = v.host_path("/evil");
        assert!(
            host.starts_with(&root),
            "末段链接逃出 rootfs：{}",
            host.display()
        );
        if let Ok(data) = std::fs::read(&host) {
            assert_ne!(data, b"HOST SECRET", "末段链接读到了宿主文件");
        }

        std::fs::remove_dir_all(&base).ok();
    }

    /// 链接成环时不能死循环，也不能返回一个"解到一半"的路径。
    #[cfg(unix)]
    #[test]
    fn symlink_loops_are_rejected_not_hung() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("wbox-vfs-loop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("rootfs");
        std::fs::create_dir_all(&root).unwrap();
        symlink("b", root.join("a")).unwrap();
        symlink("a", root.join("b")).unwrap();

        let v = vfs(Some(root.to_str().unwrap()));
        // 不挂住（有这条断言就说明没死循环），且被判为越根 -> 调用方回 EACCES
        // 必须是 ELOOP 而不是 EACCES——guest 程序按 POSIX 语义分辨这两者
        // （`tests/guest/t_path.c` 的 symlink/loop-ELOOP 就断言了这一点）。
        assert_eq!(
            v.host_path_confined("/a").unwrap_err(),
            ResolveErr::Loop,
            "成环应报 ELOOP"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// 不存在的路径照常返回（创建新文件是常态），且仍夹在 rootfs 内。
    #[test]
    fn missing_paths_still_resolve_inside_root() {
        let v = vfs(Some("/srv/rootfs"));
        assert_eq!(
            v.host_path("/no/such/file"),
            PathBuf::from("/srv/rootfs/no/such/file")
        );
        assert!(v.host_path("/../../etc/shadow").starts_with("/srv/rootfs"));
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
                v.host_path_confined(probe) == Err(ResolveErr::Escaped),
                "{probe} 应被拒绝，而不是夹到根"
            );
            assert!(v.normalize_checked(probe).1, "{probe} 应被判定为越根");
        }
        // 合法的 `..`（不弹出根）必须照常放行
        for ok in ["/usr/lib/../bin/sh", "/a/b/../c", "/."] {
            assert!(v.host_path_confined(ok).is_ok(), "{ok} 被误拒");
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
        assert!(v.host_path_confined("/..").is_ok());
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
        let a = t.alloc(Fd::new(FdKind::Closed, false, 0));
        let b = t.alloc(Fd::new(FdKind::Closed, false, 0));
        assert_eq!((a, b), (Some(3), Some(4)));
        t.remove(3);
        // Linux 保证下一个 open 拿到最小空号
        let c = t.alloc(Fd::new(FdKind::Closed, false, 0));
        assert_eq!(c, Some(3));
    }

    #[test]
    fn close_on_exec_drops_only_cloexec_fds() {
        let mut t = FdTable::new();
        let keep = t.alloc(Fd::new(FdKind::Closed, false, 0)).unwrap();
        let drop = t.alloc(Fd::new(FdKind::Closed, true, 0)).unwrap();
        t.close_on_exec();
        assert!(t.contains(keep));
        assert!(!t.contains(drop));
        assert!(t.contains(0), "标准流不带 CLOEXEC");
    }
}
