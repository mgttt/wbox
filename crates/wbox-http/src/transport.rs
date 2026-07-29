//! 传输层：TCP、HTTP 代理隧道，以及 TLS。
//!
//! TLS 走同仓的 `wbox-tls`（自实现的 TLS 1.3 客户端）。这个文件只负责
//! **把字节管道接起来**：解析地址、连 TCP、必要时打 CONNECT 隧道、
//! 再在上面套 TLS。协议与密码学都在 `wbox-tls` 里。

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// 带**整体期限**的 TCP 流。
///
/// # 为什么单靠 `set_read_timeout` 不够
///
/// 每次 read/write 的超时只能挡住"对端彻底不说话"。对端每隔 299 秒吐一个
/// 字节，每次 I/O 都在超时之内，**整体却可以拖到无限久**——一次 pull 就这么
/// 永远回不来（Windows 实机遇到过）。所以每次 I/O 之前先看总预算，把 socket
/// 超时压到 `min(单次超时, 剩余预算)`，预算用完直接报 `TimedOut`。
///
/// 期限放在 TCP 这一层而不是 HTTP 层，是因为 TLS 握手也在这条流上跑：
/// 握手里那几个"收到 CCS 就 continue"的循环同样需要一个兜底的上界，而它们
/// 在 `wbox-tls` 内部，HTTP 层够不着。
pub struct DeadlineStream {
    inner: TcpStream,
    deadline: Instant,
    io_timeout: Duration,
}

impl DeadlineStream {
    fn budget(&self) -> io::Result<Duration> {
        let now = Instant::now();
        if now >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "整体超时：连接在预算内没有完成",
            ));
        }
        Ok((self.deadline - now).min(self.io_timeout))
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let d = self.budget()?;
        self.inner.set_read_timeout(Some(d))?;
        self.inner.read(buf)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let d = self.budget()?;
        self.inner.set_write_timeout(Some(d))?;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// 已连上的传输通道。
pub enum Stream {
    Plain(DeadlineStream),
    Tls(Box<wbox_tls::TlsStream<DeadlineStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// 建立到 `host:port` 的 TCP 连接（可经代理），必要时再套上 TLS。
///
/// `proxy` 形如 `http://user-less-host:port`；仅用于 CONNECT 隧道与
/// 明文的绝对 URI 转发，代理本身的 TLS（https 代理）不支持。
pub fn connect(
    host: &str,
    port: u16,
    tls: bool,
    proxy: Option<&crate::url::Url>,
    connect_timeout: Duration,
    io_timeout: Duration,
    deadline: Instant,
) -> io::Result<Stream> {
    let (dial_host, dial_port) = match proxy {
        Some(p) => (p.host.as_str(), p.port),
        None => (host, port),
    };
    let tcp = dial(dial_host, dial_port, connect_timeout)?;
    tcp.set_nodelay(true).ok();
    let mut tcp = DeadlineStream {
        inner: tcp,
        deadline,
        io_timeout,
    };

    if proxy.is_some() && tls {
        // 明文代理 + https 目标：先用 CONNECT 打隧道，再在隧道里握手。
        // TLS 是端到端的，代理看不到隧道内容。
        connect_tunnel(&mut tcp, host, port)?;
    }
    if tls {
        connect_tls(tcp, host)
    } else {
        Ok(Stream::Plain(tcp))
    }
}

/// 解析 `host:port`。
///
/// **解析要另起线程**：`to_socket_addrs` 走的是系统 resolver，本身没有超时，
/// 而 resolver 卡住是真实存在的（DNS 服务器不回、Windows 上 NRPT/VPN 规则
/// 抖动）。卡住时这条 `connect` 就永远不返回，上层的任何"超时"都无从谈起。
/// 超时后那个线程会被留下自己了结——它只持有一份 `String`，且系统调用最终
/// 会自己返回，比强行中断安全。
fn resolve(host: &str, port: u16, timeout: Duration) -> io::Result<Vec<std::net::SocketAddr>> {
    use std::net::ToSocketAddrs;
    let (tx, rx) = std::sync::mpsc::channel();
    let owned = host.to_string();
    std::thread::spawn(move || {
        let r = (owned.as_str(), port)
            .to_socket_addrs()
            .map(|it| it.collect::<Vec<_>>());
        let _ = tx.send(r);
    });
    match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("解析 {host}:{port} 超时（{}s）", timeout.as_secs()),
        )),
    }
}

fn dial(host: &str, port: u16, timeout: Duration) -> io::Result<TcpStream> {
    let addrs = resolve(host, port, timeout)?;
    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("解析不出 {host}:{port} 的地址"),
        ));
    }
    let mut last = None;
    // 逐个地址试：主机常常同时有 IPv6 与 IPv4，只试第一个会在单栈网络里失败。
    for a in addrs {
        match TcpStream::connect_timeout(&a, timeout) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("连接失败")))
}

fn connect_tunnel(tcp: &mut DeadlineStream, host: &str, port: u16) -> io::Result<()> {
    let target = if host.contains(':') {
        format!("[{host}]:{port}") // IPv6 字面量
    } else {
        format!("{host}:{port}")
    };
    let req = format!(
        "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nProxy-Connection: keep-alive\r\n\r\n"
    );
    tcp.write_all(req.as_bytes())?;
    tcp.flush()?;
    // 只读到头部结束，之后的字节属于隧道，必须原样留给 TLS 层。
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if tcp.read(&mut byte)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "代理在 CONNECT 响应完成前关闭了连接",
            ));
        }
        head.push(byte[0]);
        if head.len() > 8192 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "代理 CONNECT 响应头过大",
            ));
        }
    }
    let text = String::from_utf8_lossy(&head);
    let ok = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .map(|c| c.starts_with('2'))
        .unwrap_or(false);
    if !ok {
        return Err(io::Error::other(format!(
            "代理拒绝 CONNECT {target}：{}",
            text.lines().next().unwrap_or("")
        )));
    }
    Ok(())
}

/// TLS 握手。
fn connect_tls(tcp: DeadlineStream, host: &str) -> io::Result<Stream> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        // 系统时钟早于 1970 时不猜一个值：证书有效期判断会整个失真，
        // 与其"看起来通过了"，不如明确失败。
        .map_err(|_| io::Error::other("系统时钟早于 1970，无法判断证书有效期"))?;
    let stream = wbox_tls::TlsStream::connect(tcp, host, now, extra_roots())?;
    Ok(Stream::Tls(Box::new(stream)))
}

/// `SSL_CERT_FILE` 指定的追加根证书。进程内只解析一次。
///
/// **只追加不替换**内置根：企业内网 / 抓包代理常用私有 CA，但那不该让
/// 公共信任根失效。
fn extra_roots() -> &'static [Vec<u8>] {
    use std::sync::OnceLock;
    static ROOTS: OnceLock<Vec<Vec<u8>>> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let Ok(path) = std::env::var("SSL_CERT_FILE") else {
            return Vec::new();
        };
        match std::fs::read_to_string(&path) {
            Ok(pem) => crate::pem::certificates(&pem),
            Err(e) => {
                // 显式设了却读不到，是配置错误。出声而不是静默忽略——
                // 否则用户会困惑于"我明明配了 CA 却还是证书错误"。
                eprintln!("wbox: 警告：SSL_CERT_FILE='{path}' 读取失败：{e}");
                Vec::new()
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;

    /// 起一个只回一句话的假代理，验证 CONNECT 的成功/失败两条路。
    fn fake_proxy(reply: &'static str) -> (String, u16) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                let mut r = io::BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                while r.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.ends_with("\r\n\r\n") || line == "\r\n" {
                        break;
                    }
                    line.clear();
                }
                let _ = s.write_all(reply.as_bytes());
            }
        });
        (addr.ip().to_string(), addr.port())
    }

    /// 测试用：给一条已连上的 TCP 套上宽松的期限。
    fn wrap(tcp: TcpStream, budget: Duration) -> DeadlineStream {
        DeadlineStream {
            inner: tcp,
            deadline: Instant::now() + budget,
            io_timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn tunnel_reports_proxy_refusal() {
        let (h, p) = fake_proxy("HTTP/1.1 403 Forbidden\r\n\r\n");
        let tcp = TcpStream::connect((h.as_str(), p)).unwrap();
        let mut tcp = wrap(tcp, Duration::from_secs(10));
        let e = connect_tunnel(&mut tcp, "example.com", 443).unwrap_err();
        assert!(e.to_string().contains("拒绝 CONNECT"), "{e}");
    }

    #[test]
    fn tunnel_accepts_2xx() {
        let (h, p) = fake_proxy("HTTP/1.1 200 Connection established\r\n\r\n");
        let tcp = TcpStream::connect((h.as_str(), p)).unwrap();
        let mut tcp = wrap(tcp, Duration::from_secs(10));
        connect_tunnel(&mut tcp, "example.com", 443).unwrap();
    }

    /// **L13 的核心判据**：对端一直在说话、每次读都不超时，整体仍必须有上界。
    ///
    /// 这条正是 `io_timeout` 单独存在时挡不住的形状——服务器每 50 ms 吐一个
    /// 字节、永不结束，每次 read 都成功返回，旧实现会永远读下去。
    #[test]
    fn slow_drip_peer_still_hits_the_total_budget() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                loop {
                    if s.write_all(b"x").is_err() {
                        return;
                    }
                    let _ = s.flush();
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        });
        let tcp = TcpStream::connect(addr).unwrap();
        // 预算 1 秒，单次 I/O 超时 5 秒：单次永远不会触发，只有总预算能收场。
        let mut s = wrap(tcp, Duration::from_secs(1));
        let started = Instant::now();
        let mut sink = [0u8; 64];
        let err = loop {
            match s.read(&mut sink) {
                Ok(_) => {
                    assert!(
                        started.elapsed() < Duration::from_secs(20),
                        "整体预算没有生效，读了 20 秒还在继续"
                    );
                }
                Err(e) => break e,
            }
        };
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
        assert!(err.to_string().contains("整体超时"), "{err}");
    }

    /// 反向判据：预算充裕时不能误伤——否则一个"永远立刻超时"的实现也能
    /// 让上面那条变绿。
    #[test]
    fn generous_budget_does_not_cut_a_healthy_read() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                std::thread::sleep(Duration::from_millis(200));
                let _ = s.write_all(b"hello");
            }
        });
        let tcp = TcpStream::connect(addr).unwrap();
        let mut s = wrap(tcp, Duration::from_secs(10));
        let mut buf = [0u8; 5];
        s.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello");
    }

    /// DNS 解析必须走得通（有界不等于把正常解析也挡了）。
    #[test]
    fn resolve_returns_loopback_within_budget() {
        let addrs = resolve("127.0.0.1", 80, Duration::from_secs(5)).unwrap();
        assert!(!addrs.is_empty());
    }

    #[test]
    fn builtin_roots_are_present() {
        // 内置根为空的话所有 https 都会失败，且错误信息只说"回溯不到根"。
        assert!(wbox_tls::roots::TRUSTED_ROOTS.len() > 100);
    }
}
