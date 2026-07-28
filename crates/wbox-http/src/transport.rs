//! 传输层：TCP、HTTP 代理隧道，以及 TLS。
//!
//! # 这个文件是仓库里唯一还有第三方密码学的地方
//!
//! 协议层（`url` / `wire` / `client`）已经全是第一方实现。TLS 的握手与
//! 密码学原语仍是 `rustls` + `rustls-rustcrypto`，原因写在
//! `docs/rust-rewrite.md` §5：自己写 TLS 意味着自己写 X25519、AES-GCM、
//! RSA/ECDSA 验签与 X.509 链校验，那是**未经审计、非常量时间**的密码学，
//! 与"少一个第三方 crate"要放在一起权衡，属于需要人拍板的取舍。
//!
//! 接缝就在 [`Stream`]：换成第一方 TLS 时只动这个文件里的 `connect_tls`。

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

/// 已连上的传输通道。
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
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
    let req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nProxy-Connection: keep-alive\r\n\r\n");
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

/// TLS 握手。**这是第三方密码学的唯一入口**，见文件顶部说明。
fn connect_tls(tcp: TcpStream, host: &str) -> io::Result<Stream> {
    let config = tls_config();
    let server = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| io::Error::other(format!("TLS 服务器名非法：{host}")))?;
    let conn = rustls::ClientConnection::new(config, server)
        .map_err(|e| io::Error::other(format!("TLS 初始化失败：{e}")))?;
    Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))))
}

/// 进程内共享一份 TLS 配置：根证书解析只做一次。
fn tls_config() -> Arc<rustls::ClientConfig> {
    use std::sync::OnceLock;
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = rustls::RootCertStore::empty();
            // 内置根证书是**纯 Rust 数据**（不读系统证书库），保证 portable
            // 分发在任何机器上行为一致。
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            // 企业内网 / 抓包代理常常用私有 CA。沿用 OpenSSL 的惯例变量，
            // 让用户能显式追加而不必改代码。只追加，不替换内置根。
            if let Ok(path) = std::env::var("SSL_CERT_FILE") {
                if let Ok(pem) = std::fs::read_to_string(&path) {
                    for der in crate::pem::certificates(&pem) {
                        let _ = roots.add(rustls::pki_types::CertificateDer::from(der));
                    }
                }
            }
            let provider = Arc::new(rustls_rustcrypto::provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("默认协议版本集合有效")
                .with_root_certificates(roots)
                .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
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
    fn tls_config_builds_with_roots() {
        // 构造失败会 panic 在 expect 上；这条同时确认根证书不为空。
        let _ = tls_config();
        assert!(!webpki_roots::TLS_SERVER_ROOTS.is_empty());
    }
}
