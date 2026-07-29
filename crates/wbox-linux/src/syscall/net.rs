//! AF_UNIX 套接字与 epoll。
//!
//! # 为什么是"进程内实现"而不是转给宿主
//!
//! 宿主可能是 Windows，那里根本没有 AF_UNIX 的等价物（Win10 1803 起的
//! `AF_UNIX` 只支持路径式、不支持 `socketpair`），而 `socketpair` 恰恰是
//! guest 侧最常用的一种——libc 的很多设施、以及本仓 `t_net_epoll` 的全部
//! 用例都建立在它上面。
//!
//! 更根本的理由是**语义要在两个宿主上一致**：PRD F5 要求同一条命令在
//! Windows 与 Linux 上表现相同。把 AF_UNIX 做成引擎自己的进程内对象，
//! 两边就是同一份代码、同一套行为，不必再去追平两个操作系统的差异。
//! 代价说清楚：这样的 AF_UNIX **只在同一个 wbox-linux 进程内连得通**，
//! 跨进程（guest 连宿主上别的程序的 unix socket）连不上。快照式 fork 的
//! 父子在同一个宿主进程里，所以 fork 之后照样通。
//!
//! AF_INET/AF_INET6 是另一回事——它们必须真的走网络，见 `sys_socket` 的说明。

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;

pub const AF_UNIX: i32 = 1;
pub const AF_INET: i32 = 2;
pub const AF_INET6: i32 = 10;

pub const SOCK_STREAM: i32 = 1;
pub const SOCK_DGRAM: i32 = 2;

/// `socket()` / `socketpair()` / `accept4()` 的类型位里夹带的 fd 标志。
pub const SOCK_NONBLOCK: i32 = 0o4000;
pub const SOCK_CLOEXEC: i32 = 0o2000000;

pub const SHUT_RD: i32 = 0;
pub const SHUT_WR: i32 = 1;
pub const SHUT_RDWR: i32 = 2;

/// 单向字节流。一条 AF_UNIX 流式连接由两条组成，方向相反。
///
/// `closed` 表示**写侧**已经没了（对端 socket 全部析构，或对端
/// `shutdown(SHUT_WR)`）。读侧据此把"暂时没数据"（`EAGAIN`）和"再也不会有
/// 数据了"（返回 0 = EOF，poll 报 `HUP`）区分开——少了这一位，`epoll` 会在
/// 已断开的连接上永远报"没就绪"，调用方就挂住了。
pub struct Chan {
    pub data: RefCell<VecDeque<u8>>,
    pub closed: Cell<bool>,
    pub capacity: Cell<usize>,
    /// 写入代次，语义同 `fs::PipeInner::epoch`（边沿触发要用）。
    pub epoch: Cell<u64>,
}

impl Chan {
    fn new() -> Rc<Self> {
        Rc::new(Chan {
            data: RefCell::new(VecDeque::new()),
            closed: Cell::new(false),
            capacity: Cell::new(64 * 1024),
            epoch: Cell::new(0),
        })
    }

    pub fn space(&self) -> usize {
        self.capacity.get().saturating_sub(self.data.borrow().len())
    }
}

/// 数据报队列。与 [`Chan`] 的区别只有一个但很关键：**保留消息边界**。
pub struct DgramChan {
    pub msgs: RefCell<VecDeque<Vec<u8>>>,
    pub closed: Cell<bool>,
    pub epoch: Cell<u64>,
}

impl DgramChan {
    fn new() -> Rc<Self> {
        Rc::new(DgramChan {
            msgs: RefCell::new(VecDeque::new()),
            closed: Cell::new(false),
            epoch: Cell::new(0),
        })
    }
}

/// 一条已建立连接在**本端**看到的两个方向。
pub enum Conn {
    Stream {
        rx: Rc<Chan>,
        tx: Rc<Chan>,
    },
    Dgram {
        rx: Rc<DgramChan>,
        tx: Rc<DgramChan>,
    },
}

impl Conn {
    /// 造一对互为对端的连接。
    fn pair(stream: bool) -> (Conn, Conn) {
        if stream {
            let a = Chan::new();
            let b = Chan::new();
            (
                Conn::Stream {
                    rx: Rc::clone(&a),
                    tx: Rc::clone(&b),
                },
                Conn::Stream { rx: b, tx: a },
            )
        } else {
            let a = DgramChan::new();
            let b = DgramChan::new();
            (
                Conn::Dgram {
                    rx: Rc::clone(&a),
                    tx: Rc::clone(&b),
                },
                Conn::Dgram { rx: b, tx: a },
            )
        }
    }

    fn clone_handle(&self) -> Conn {
        match self {
            Conn::Stream { rx, tx } => Conn::Stream {
                rx: Rc::clone(rx),
                tx: Rc::clone(tx),
            },
            Conn::Dgram { rx, tx } => Conn::Dgram {
                rx: Rc::clone(rx),
                tx: Rc::clone(tx),
            },
        }
    }

    /// 关掉本端的**写**方向：对端从此读到 EOF。
    fn close_tx(&self) {
        match self {
            Conn::Stream { tx, .. } => tx.closed.set(true),
            Conn::Dgram { tx, .. } => tx.closed.set(true),
        }
    }

    fn rx_closed(&self) -> bool {
        match self {
            Conn::Stream { rx, .. } => rx.closed.get(),
            Conn::Dgram { rx, .. } => rx.closed.get(),
        }
    }

    fn rx_ready(&self) -> bool {
        match self {
            Conn::Stream { rx, .. } => !rx.data.borrow().is_empty(),
            Conn::Dgram { rx, .. } => !rx.msgs.borrow().is_empty(),
        }
    }

    fn tx_ready(&self) -> bool {
        match self {
            Conn::Stream { tx, .. } => !tx.closed.get() && tx.space() > 0,
            Conn::Dgram { tx, .. } => !tx.closed.get(),
        }
    }
}

/// 监听中的 AF_UNIX socket 的共享状态。
///
/// `pending` 里放的是**服务端一侧**的连接：`connect` 当场把两端造好，
/// 服务端那一半排进这里等 `accept` 取走。单线程模型下这是唯一说得通的
/// 做法——没有第二个线程能在 `accept` 阻塞期间完成三次握手。
pub struct Listener {
    pub backlog: Cell<usize>,
    pub pending: RefCell<VecDeque<Conn>>,
    /// 绑定的宿主路径（匿名监听为 `None`）。
    pub path: RefCell<Option<PathBuf>>,
}

pub enum SockState {
    /// 刚 `socket()`，既没 bind 也没 connect。
    Unbound,
    /// 已 `bind` 到路径，还没 `listen`。
    Bound(PathBuf),
    Listening(Rc<Listener>),
    Connected(Conn),
}

/// 一个套接字对象。
///
/// **被 `Rc` 持有**：`dup`/`dup2`/`fork` 出来的别名指向同一个对象，所以
/// "还有几个 fd 引用着它"是自动记账的。最后一个别名析构时 `Drop` 把发送
/// 方向标成关闭——对端这才读到 EOF。用 `Rc` 的引用计数而不是自己数 fd，
/// 理由与管道那边一样：fd 表消失的路径有好几条，手工记账一定会漏。
pub struct Socket {
    pub domain: i32,
    pub sotype: i32,
    pub state: RefCell<SockState>,
    /// `(level, name) -> value`。原样存原样取；不做任何"真的生效"的假装。
    pub opts: RefCell<Vec<((i32, i32), i32)>>,
    pub shut_rd: Cell<bool>,
    pub shut_wr: Cell<bool>,
    /// AF_INET/AF_INET6 的宿主侧状态；AF_UNIX 恒为 `Inet::Idle`。
    pub inet: RefCell<Inet>,
    /// 上一次后台 connect 的结果（`SO_ERROR` 取一次就清）。
    pub so_error: Cell<i32>,
}

impl Socket {
    pub fn new(domain: i32, sotype: i32) -> Rc<Socket> {
        Rc::new(Socket {
            domain,
            sotype,
            state: RefCell::new(SockState::Unbound),
            opts: RefCell::new(Vec::new()),
            shut_rd: Cell::new(false),
            shut_wr: Cell::new(false),
            inet: RefCell::new(Inet::Idle),
            so_error: Cell::new(0),
        })
    }

    pub fn is_stream(&self) -> bool {
        self.sotype == SOCK_STREAM
    }

    pub fn is_inet(&self) -> bool {
        self.domain == AF_INET || self.domain == AF_INET6
    }

    /// 把后台 connect 的结果收进来（非阻塞）。返回是否刚刚完成。
    pub fn poll_connect(&self) -> bool {
        let done = {
            let st = self.inet.borrow();
            let Inet::Connecting(rx) = &*st else {
                return false;
            };
            rx.try_recv().ok()
        };
        match done {
            Some(Ok(s)) => {
                *self.inet.borrow_mut() = Inet::Stream(s);
                true
            }
            Some(Err(e)) => {
                self.so_error.set(
                    e.raw_os_error()
                        .unwrap_or(crate::syscall::ECONNREFUSED as i32),
                );
                *self.inet.borrow_mut() = Inet::Idle;
                true
            }
            None => false,
        }
    }

    /// AF_INET 的就绪位。
    pub fn inet_readiness(&self) -> u32 {
        self.poll_connect();
        let st = self.inet.borrow();
        match &*st {
            // 监听中：`accept` 会不会阻塞，只能真去试一次才知道，而试一次
            // 就把连接取走了。这里保守报可读——调用方随后的 `accept` 拿到
            // EAGAIN 是合法的（POSIX 明确允许 poll 报就绪后 accept 仍 EAGAIN）。
            Inet::Listener(_) => EPOLLIN,
            Inet::Stream(_) => EPOLLIN | EPOLLOUT,
            // connect 还没完成：既不可读也不可写，正是 POLLOUT 要等的那个状态。
            Inet::Connecting(_) => 0,
            Inet::Udp(_) => EPOLLIN | EPOLLOUT,
            Inet::Idle => EPOLLOUT,
        }
    }

    /// 接收方向的写入代次（对端每写一次就变）。
    pub fn rx_epoch(&self) -> u64 {
        match &*self.state.borrow() {
            SockState::Connected(Conn::Stream { rx, .. }) => rx.epoch.get(),
            SockState::Connected(Conn::Dgram { rx, .. }) => rx.epoch.get(),
            SockState::Listening(l) => l.pending.borrow().len() as u64,
            _ => 0,
        }
    }

    /// 就绪位（epoll 口径；`poll` 侧再映射一次）。
    pub fn readiness(&self) -> u32 {
        if self.is_inet() {
            return self.inet_readiness();
        }
        let st = self.state.borrow();
        match &*st {
            SockState::Listening(l) => {
                // 有等待中的连接 = 可读（`accept` 不会阻塞）。
                if l.pending.borrow().is_empty() {
                    0
                } else {
                    EPOLLIN
                }
            }
            SockState::Connected(c) => {
                let mut r = 0;
                let eof = c.rx_closed();
                if c.rx_ready() || eof || self.shut_rd.get() {
                    r |= EPOLLIN;
                }
                if eof {
                    // 对端不再发数据：RDHUP。两端都断了才是 HUP。
                    r |= EPOLLRDHUP;
                    if c.tx_closed_by_peer() {
                        r |= EPOLLHUP;
                    }
                }
                if c.tx_ready() {
                    r |= EPOLLOUT;
                } else if !c.tx_ready() && c.tx_is_closed() {
                    r |= EPOLLHUP;
                }
                r
            }
            // 未连接的 socket 不可读；可写与否没有意义，按 Linux 报可写。
            _ => EPOLLOUT,
        }
    }
}

impl Conn {
    fn tx_is_closed(&self) -> bool {
        match self {
            Conn::Stream { tx, .. } => tx.closed.get(),
            Conn::Dgram { tx, .. } => tx.closed.get(),
        }
    }

    /// 对端已经把**我们的发送通道**也丢了（对端 socket 整个析构）。
    ///
    /// 判据是 `Rc` 的强引用只剩我们自己这一份——对端还活着的话它手里必有
    /// 一份。这正是"用引用计数代替手工记账"的兑现处。
    fn tx_closed_by_peer(&self) -> bool {
        match self {
            Conn::Stream { tx, .. } => Rc::strong_count(tx) == 1,
            Conn::Dgram { tx, .. } => Rc::strong_count(tx) == 1,
        }
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        if let SockState::Connected(c) = &*self.state.borrow() {
            c.close_tx();
        }
    }
}

// ------------------------------------------------------------------ 操作

/// `socketpair(AF_UNIX, ...)`：直接造一对已连接的 socket。
pub fn socketpair(sotype: i32) -> (Rc<Socket>, Rc<Socket>) {
    let stream = sotype == SOCK_STREAM;
    let (a, b) = Conn::pair(stream);
    let sa = Socket::new(AF_UNIX, sotype);
    let sb = Socket::new(AF_UNIX, sotype);
    *sa.state.borrow_mut() = SockState::Connected(a);
    *sb.state.borrow_mut() = SockState::Connected(b);
    (sa, sb)
}

// 进程内的"已绑定路径 -> 监听者"注册表。
//
// 用 `thread_local` 而不是塞进 `Os`：快照式 fork 会整份复制 `Os`，那样
// 父子就各有一张表、子进程 bind 的路径父进程连不上。放在线程局部反而
// 与"同一个宿主进程内可见"这条语义对齐。
thread_local! {
    static REGISTRY: RefCell<Vec<(PathBuf, std::rc::Weak<Listener>)>> =
        const { RefCell::new(Vec::new()) };
}

pub fn register_listener(path: PathBuf, l: &Rc<Listener>) {
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        r.retain(|(p, w)| w.strong_count() > 0 && p != &path);
        r.push((path, Rc::downgrade(l)));
    });
}

pub fn lookup_listener(path: &std::path::Path) -> Option<Rc<Listener>> {
    REGISTRY.with(|r| {
        r.borrow()
            .iter()
            .find(|(p, _)| p == path)
            .and_then(|(_, w)| w.upgrade())
    })
}

pub fn unregister_listener(path: &std::path::Path) {
    REGISTRY.with(|r| {
        r.borrow_mut()
            .retain(|(p, w)| p != path && w.strong_count() > 0)
    });
}

/// 主动连接一个监听者。成功时返回**客户端一侧**的连接。
pub fn connect_to(l: &Rc<Listener>, stream: bool) -> Result<Conn, i64> {
    let mut q = l.pending.borrow_mut();
    // backlog 满：Linux 上阻塞式 connect 会等，非阻塞报 EAGAIN。单线程下
    // 没人能在我们等待期间 accept，所以一律报 ECONNREFUSED——挂住才是错的。
    if q.len() >= l.backlog.get().max(1) {
        return Err(-crate::syscall::ECONNREFUSED);
    }
    let (server, client) = Conn::pair(stream);
    q.push_back(server);
    Ok(client)
}

/// 从监听队列里取一条已排好的连接。
pub fn accept_from(l: &Rc<Listener>) -> Option<Conn> {
    l.pending.borrow_mut().pop_front()
}

/// AF_INET 的读写：直接转给宿主套接字，按 guest 的 `O_NONBLOCK` 设置模式。
///
/// 每次操作前都设一遍而不是建连接时设一次：`O_NONBLOCK` 属于打开文件描述，
/// guest 随时可以用 `fcntl`/`ioctl` 翻转它，而那两条路径改的是我们自己的
/// 状态位，宿主 socket 并不知道。
pub fn inet_io(s: &Socket, nonblock: bool) -> Result<std::net::TcpStream, i64> {
    s.poll_connect();
    let st = s.inet.borrow();
    match &*st {
        Inet::Stream(t) => {
            let _ = t.set_nonblocking(nonblock);
            t.try_clone().map_err(|_| -crate::syscall::EIO)
        }
        Inet::Connecting(_) => Err(-crate::syscall::EAGAIN),
        _ => Err(-crate::syscall::ENOTCONN),
    }
}

/// 读。返回 `Err(errno)` 表示要报错（含 `EAGAIN`）。
pub fn recv(s: &Socket, buf: &mut [u8], peek: bool) -> Result<usize, i64> {
    if s.shut_rd.get() {
        return Ok(0);
    }
    let st = s.state.borrow();
    let SockState::Connected(c) = &*st else {
        return Err(-crate::syscall::ENOTCONN);
    };
    match c {
        Conn::Stream { rx, .. } => {
            let mut q = rx.data.borrow_mut();
            if q.is_empty() {
                if rx.closed.get() {
                    return Ok(0); // 对端已关：EOF
                }
                return Err(-crate::syscall::EAGAIN);
            }
            let k = buf.len().min(q.len());
            if peek {
                for (i, b) in q.iter().take(k).enumerate() {
                    buf[i] = *b;
                }
            } else {
                for (i, b) in q.drain(..k).enumerate() {
                    buf[i] = b;
                }
            }
            Ok(k)
        }
        Conn::Dgram { rx, .. } => {
            let mut q = rx.msgs.borrow_mut();
            let Some(front) = q.front().cloned() else {
                if rx.closed.get() {
                    return Ok(0);
                }
                return Err(-crate::syscall::EAGAIN);
            };
            // 数据报语义：一次收一条，收不下的部分**丢掉**（不留到下次）。
            let k = buf.len().min(front.len());
            buf[..k].copy_from_slice(&front[..k]);
            if !peek {
                q.pop_front();
            }
            Ok(k)
        }
    }
}

/// 写。
pub fn send(s: &Socket, data: &[u8], nonblock: bool) -> Result<usize, i64> {
    if s.shut_wr.get() {
        return Err(-crate::syscall::EPIPE);
    }
    let st = s.state.borrow();
    let SockState::Connected(c) = &*st else {
        return Err(-crate::syscall::ENOTCONN);
    };
    match c {
        Conn::Stream { tx, .. } => {
            if tx.closed.get() || Rc::strong_count(tx) == 1 {
                return Err(-crate::syscall::EPIPE);
            }
            if nonblock {
                let space = tx.space();
                if space == 0 {
                    return Err(-crate::syscall::EAGAIN);
                }
                let k = space.min(data.len());
                tx.data.borrow_mut().extend(data[..k].iter().copied());
                tx.epoch.set(tx.epoch.get().wrapping_add(1));
                Ok(k)
            } else {
                // 容量只对非阻塞写强制，理由同管道（单线程下阻塞必死锁）。
                tx.data.borrow_mut().extend(data.iter().copied());
                tx.epoch.set(tx.epoch.get().wrapping_add(1));
                Ok(data.len())
            }
        }
        Conn::Dgram { tx, .. } => {
            if tx.closed.get() || Rc::strong_count(tx) == 1 {
                return Err(-crate::syscall::EPIPE);
            }
            tx.msgs.borrow_mut().push_back(data.to_vec());
            tx.epoch.set(tx.epoch.get().wrapping_add(1));
            Ok(data.len())
        }
    }
}

/// `ioctl(FIONREAD)`：可立即读出的字节数。
pub fn readable_bytes(s: &Socket) -> usize {
    let st = s.state.borrow();
    match &*st {
        SockState::Connected(Conn::Stream { rx, .. }) => rx.data.borrow().len(),
        SockState::Connected(Conn::Dgram { rx, .. }) => {
            rx.msgs.borrow().front().map(|m| m.len()).unwrap_or(0)
        }
        _ => 0,
    }
}

pub fn shutdown(s: &Socket, how: i32) -> i64 {
    let st = s.state.borrow();
    if !matches!(&*st, SockState::Connected(_)) {
        return -crate::syscall::ENOTCONN;
    }
    if how == SHUT_RD || how == SHUT_RDWR {
        s.shut_rd.set(true);
    }
    if how == SHUT_WR || how == SHUT_RDWR {
        s.shut_wr.set(true);
        if let SockState::Connected(c) = &*st {
            c.close_tx();
        }
    }
    0
}

pub fn clone_state_for_accept(c: Conn) -> Rc<Socket> {
    let s = Socket::new(AF_UNIX, SOCK_STREAM);
    *s.state.borrow_mut() = SockState::Connected(c);
    s
}

pub fn conn_handle(s: &Socket) -> Option<Conn> {
    match &*s.state.borrow() {
        SockState::Connected(c) => Some(c.clone_handle()),
        _ => None,
    }
}

// ------------------------------------------------------------------ epoll

pub const EPOLLIN: u32 = 0x001;
pub const EPOLLOUT: u32 = 0x004;
pub const EPOLLERR: u32 = 0x008;
pub const EPOLLHUP: u32 = 0x010;
pub const EPOLLRDHUP: u32 = 0x2000;
pub const EPOLLONESHOT: u32 = 1 << 30;
pub const EPOLLET: u32 = 1 << 31;

/// epoll 关注项指向的**对象**。
///
/// 关键在于它不是 fd 号。真内核按"打开文件描述"索引：注册之后把那个 fd 号
/// `close` 掉，只要还有别名活着，事件就照报（`epoll/watched-fd-dup-alias`
/// 断言的正是这条）。只记 fd 号的话，`close` 之后查表就查空了。
///
/// 所以这里直接把底层共享对象的句柄存下来，由它自己决定就绪与否。
/// 一律用 `Weak`：**关注项不能让被监视的对象活下去**。
///
/// 真内核在"指向该描述的最后一个 fd 关闭"时自动摘掉关注项。若这里持强引用，
/// 对象永远不死、永远"就绪"，于是前一组用例关掉的 socket 会在后一组的
/// `epoll_wait` 里继续冒事件（实测：`epoll/watched-fd-dup-alias` 收到 2 个
/// 事件，其中一个来自早已关闭的 `epoll/multi-fd`）。用 `Weak` 之后，
/// upgrade 失败就等于"该描述已经没人引用了"，正是内核那条规则。
///
/// 管道要多一步：`PipeInner` 由读写两端共同持有，只看 `Weak` 活不活分不出
/// "读端关了但写端还在"。所以另看读/写端计数。
pub enum Target {
    Socket(std::rc::Weak<Socket>),
    PipeRead(std::rc::Weak<crate::syscall::fs::PipeInner>),
    PipeWrite(std::rc::Weak<crate::syscall::fs::PipeInner>),
    Event(std::rc::Weak<crate::syscall::fs::EventFd>),
    /// 普通文件、目录、标准流、合成设备：Linux 上恒可读可写。
    AlwaysReady,
}

impl Target {
    /// 被监视的描述是否还有人引用。
    pub fn alive(&self) -> bool {
        match self {
            Target::Socket(w) => w.strong_count() > 0,
            Target::PipeRead(w) => w.upgrade().is_some_and(|i| !i.readers_closed()),
            Target::PipeWrite(w) => w.upgrade().is_some_and(|i| !i.writers_closed()),
            Target::Event(w) => w.strong_count() > 0,
            Target::AlwaysReady => true,
        }
    }

    /// 就绪状态的"代次"：只要它变了，就说明**发生过一次新的就绪事件**，
    /// 边沿触发据此重新武装。见 `fs::PipeInner::epoch` 的说明。
    pub fn epoch(&self) -> u64 {
        match self {
            Target::Socket(w) => w.upgrade().map(|s| s.rx_epoch()).unwrap_or(0),
            Target::PipeRead(w) | Target::PipeWrite(w) => {
                w.upgrade().map(|i| i.epoch()).unwrap_or(0)
            }
            Target::Event(w) => w.upgrade().map(|e| e.epoch.get()).unwrap_or(0),
            Target::AlwaysReady => 0,
        }
    }

    /// 两个关注项是否指向**同一个底层对象**。
    ///
    /// `EPOLL_CTL_ADD` 判 `EEXIST` 要看的是这个，不是 fd 号：fd 号会被
    /// `close` 之后回收再分配，只比号会把"新对象用了旧号"误判成重复注册。
    pub fn same(&self, other: &Target) -> bool {
        match (self, other) {
            (Target::Socket(a), Target::Socket(b)) => std::rc::Weak::ptr_eq(a, b),
            (Target::PipeRead(a), Target::PipeRead(b)) => std::rc::Weak::ptr_eq(a, b),
            (Target::PipeWrite(a), Target::PipeWrite(b)) => std::rc::Weak::ptr_eq(a, b),
            (Target::Event(a), Target::Event(b)) => std::rc::Weak::ptr_eq(a, b),
            _ => false,
        }
    }

    pub fn readiness(&self) -> u32 {
        match self {
            Target::Socket(w) => w.upgrade().map(|s| s.readiness()).unwrap_or(0),
            Target::PipeRead(w) => {
                let Some(inner) = w.upgrade() else { return 0 };
                let mut v = 0;
                let hup = inner.writers_closed();
                if !inner.data.borrow().is_empty() || hup {
                    v |= EPOLLIN;
                }
                if hup {
                    v |= EPOLLHUP;
                }
                v
            }
            Target::PipeWrite(w) => {
                let Some(inner) = w.upgrade() else { return 0 };
                if inner.readers_closed() {
                    EPOLLERR
                } else if inner.space() > 0 {
                    EPOLLOUT
                } else {
                    0
                }
            }
            Target::Event(w) => {
                let Some(e) = w.upgrade() else { return 0 };
                let mut v = 0;
                if e.counter.get() > 0 {
                    v |= EPOLLIN;
                }
                if e.counter.get() < crate::syscall::fs::EVENTFD_MAX {
                    v |= EPOLLOUT;
                }
                v
            }
            Target::AlwaysReady => EPOLLIN | EPOLLOUT,
        }
    }
}

/// 一条 epoll 关注项。
pub struct Interest {
    /// 注册时用的 fd 号。**只用于 `EPOLL_CTL_MOD`/`DEL` 的查找**，
    /// 就绪判定一律走 [`Interest::target`]——见 [`Target`] 的说明。
    pub fd: i32,
    pub target: Target,
    pub events: u32,
    pub data: u64,
    /// 边沿触发（`EPOLLET`）/ 一次性（`EPOLLONESHOT`）的已报状态。
    /// `Some(mask)` = 上次报过这些位，还没回到"未就绪"。
    pub fired: Cell<u32>,
    /// `EPOLLONESHOT` 已触发，需要 `MOD` 才重新武装。
    pub disarmed: Cell<bool>,
    /// 上次上报时看到的就绪代次（边沿触发用）。
    pub seen_epoch: Cell<u64>,
}

pub struct Epoll {
    pub interests: RefCell<Vec<Interest>>,
}

impl Epoll {
    pub fn new() -> Rc<Epoll> {
        Rc::new(Epoll {
            interests: RefCell::new(Vec::new()),
        })
    }
}

// ------------------------------------------------------- AF_INET / AF_INET6
//
// 这一族**必须真的走网络**，所以只能落到宿主套接字上。与 AF_UNIX 相反：
// 那边要的是"两个宿主行为一致"，这边要的是"真能连上外面"，而后者宿主已经
// 提供了跨平台实现（`std::net`），自己再实现一遍 TCP 既不可能也没意义。

/// AF_INET 套接字的宿主侧状态。
pub enum Inet {
    /// 刚 `socket()`，还没 bind/connect。
    Idle,
    Listener(std::net::TcpListener),
    Stream(std::net::TcpStream),
    /// 非阻塞 `connect` 正在后台进行。
    ///
    /// `std` 没有非阻塞 connect，而 `EINPROGRESS` 是这条路径上唯一正确的
    /// 答案——libc 与几乎所有网络库都靠它来判断"要不要去 poll 可写"。
    /// 与其为它写两套平台原生代码，不如把阻塞 connect 丢到一个线程里：
    /// 调用方立刻拿到 `EINPROGRESS`，之后 `poll(POLLOUT)`／`SO_ERROR`
    /// 再来取结果。行为对得上，且两个宿主同一份实现。
    Connecting(std::sync::mpsc::Receiver<std::io::Result<std::net::TcpStream>>),
    Udp(std::net::UdpSocket),
}

impl Inet {
    /// 已连上的流（含刚刚完成的后台 connect）。
    pub fn stream(&self) -> Option<&std::net::TcpStream> {
        match self {
            Inet::Stream(s) => Some(s),
            _ => None,
        }
    }
}

/// 后台 connect：一次性通道，线程做完就送结果。
pub fn spawn_connect(addr: std::net::SocketAddr) -> Inet {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(std::net::TcpStream::connect(addr));
    });
    Inet::Connecting(rx)
}
