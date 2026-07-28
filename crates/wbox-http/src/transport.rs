//! 传输层：TCP、HTTP 代理隧道，以及 TLS。
//!
//! TLS 走同仓的 `wbox-tls`（自实现的 TLS 1.3 客户端）。这个文件只负责
//! **把字节管道接起来**：解析地址、连 TCP、必要时打 CONNECT 隧道、
//! 再在上面套 TLS。协议与密码学都在 `wbox-tls` 里。

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// 已连上的传输通道。
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<wbox_tls::TlsStream<TcpStream>>),
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
) -> io::Result<Stream> {
    let (dial_host, dial_port) = match proxy {
        Some(p) => (p.host.as_str(), p.port),
        None => (host, port),
    };
    let tcp = dial(dial_host, dial_port, connect_timeout)?;
    tcp.set_read_timeout(Some(io_timeout))?;
    tcp.set_write_timeout(Some(io_timeout))?;
    tcp.set_nodelay(true).ok();

    let mut tcp = tcp;
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

fn dial(host: &str, port: u16, timeout: Duration) -> io::Result<TcpStream> {
    use std::net::ToSocketAddrs;
    let addrs: Vec<_> = (host, port).to_socket_addrs()?.collect();
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

fn connect_tunnel(tcp: &mut TcpStream, host: &str, port: u16) -> io::Result<()> {
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
fn connect_tls(tcp: TcpStream, host: &str) -> io::Result<Stream> {
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

    #[test]
    fn tunnel_reports_proxy_refusal() {
        let (h, p) = fake_proxy("HTTP/1.1 403 Forbidden\r\n\r\n");
        let mut tcp = TcpStream::connect((h.as_str(), p)).unwrap();
        let e = connect_tunnel(&mut tcp, "example.com", 443).unwrap_err();
        assert!(e.to_string().contains("拒绝 CONNECT"), "{e}");
    }

    #[test]
    fn tunnel_accepts_2xx() {
        let (h, p) = fake_proxy("HTTP/1.1 200 Connection established\r\n\r\n");
        let mut tcp = TcpStream::connect((h.as_str(), p)).unwrap();
        connect_tunnel(&mut tcp, "example.com", 443).unwrap();
    }

    #[test]
    fn builtin_roots_are_present() {
        // 内置根为空的话所有 https 都会失败，且错误信息只说"回溯不到根"。
        assert!(wbox_tls::roots::TRUSTED_ROOTS.len() > 100);
    }
}
