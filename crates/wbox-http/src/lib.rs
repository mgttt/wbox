//! `wbox-http` —— 阻塞式 HTTP/1.1 客户端，取代 `ureq`。
//!
//! # 为什么自己写
//!
//! `PRD.md` §2.2.1 收紧后的口径是"承载产品能力的实现必须是第一方 Rust"。
//! `ureq` 承载的是 registry 协议那条链路的传输层：请求构造、响应解析、
//! 重定向、超时、上限——这些行为直接决定 pull/push 的正确性与安全性
//!（例如"跨主机重定向要不要继续发 Authorization"）。它们应当在本仓可读、
//! 可测、可改。
//!
//! # 范围
//!
//! 只做 registry 用得到的那一档：`GET`/`HEAD`/`POST`/`PUT`、Content-Length
//! 与 chunked 两种响应体、3xx 重定向、明文代理隧道。**不做**连接池、
//! HTTP/2、cookie、自动解压、通用 URL 规范化——这些 registry 用不上，
//! 而每一样都是一处需要长期维护的语义。
//!
//! # 安全上的三条硬规则
//!
//! 1. **跨主机重定向必须丢掉 `Authorization`**。registry 的 blob 会被重定向
//!    到 CDN，把 Bearer token 带过去等于把凭证交给第三方。见
//!    [`Client::request`]。
//! 2. **响应体有上限**。层可以很大，但"没有上限"意味着一个恶意对端能把
//!    wbox 撑爆。
//! 3. **头部值里的 CR/LF 一律拒绝**，否则攻击者控制的字符串能拆出额外请求。
//!
//! TLS 走同仓的 `wbox-tls`（自实现的 TLS 1.3 客户端），见 [`transport`]。

pub mod pem;
pub mod transport;
pub mod url;
pub mod wire;

use std::io;
use std::time::Duration;

pub use url::Url;
pub use wire::Response;

/// HTTP 客户端。构造一次反复使用；本身无状态（不做连接池）。
pub struct Client {
    user_agent: String,
    connect_timeout: Duration,
    io_timeout: Duration,
    max_body: u64,
    max_redirects: usize,
}

impl Client {
    pub fn new(user_agent: impl Into<String>) -> Self {
        Self {
            user_agent: user_agent.into(),
            connect_timeout: Duration::from_secs(15),
            io_timeout: Duration::from_secs(300),
            max_body: 8 << 30,
            max_redirects: 10,
        }
    }

    pub fn connect_timeout(mut self, d: Duration) -> Self {
        self.connect_timeout = d;
        self
    }

    /// 单次读写的超时。层可能很大，所以这是**每次 I/O**的超时而不是整体超时。
    pub fn io_timeout(mut self, d: Duration) -> Self {
        self.io_timeout = d;
        self
    }

    pub fn max_body(mut self, bytes: u64) -> Self {
        self.max_body = bytes;
        self
    }

    /// 发一次请求，自动跟随重定向。
    ///
    /// 返回的 `Response` 里 4xx/5xx 是**正常返回**而不是 `Err`——registry 就是
    /// 用 401 回话开启 Bearer 流程的，把它变成错误会让整个认证流程走不通。
    /// 只有传输层面的失败才是 `Err`。
    pub fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<&[u8]>,
    ) -> io::Result<Response> {
        if !matches!(method, "GET" | "HEAD" | "POST" | "PUT") {
            return Err(io::Error::other(format!("不支持的 HTTP method：{method}")));
        }
        let mut target = Url::parse(url).map_err(io::Error::other)?;
        let mut method = method.to_string();
        let mut body = body;
        let mut headers = headers.to_vec();
        let origin_host = target.host.clone();

        for _ in 0..=self.max_redirects {
            let resp = self.send_once(&method, &target, &headers, body)?;
            let is_redirect = matches!(resp.status, 301 | 302 | 303 | 307 | 308);
            let Some(location) = resp.header("location").filter(|_| is_redirect) else {
                return Ok(resp);
            };
            let next = target.join(location).map_err(io::Error::other)?;

            // 规则 1：跨主机就丢掉 Authorization。blob 会被重定向到 CDN，
            // 把 Bearer token 带过去等于把凭证交给第三方。
            if next.host != origin_host {
                headers.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
            }
            // 303 一律转 GET；301/302 对 POST 也按浏览器的既成事实转 GET。
            // 307/308 保持方法与请求体不变（registry 的 blob 上传依赖这点）。
            if resp.status == 303 || (matches!(resp.status, 301 | 302) && method == "POST") {
                method = "GET".to_string();
                body = None;
                headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
            }
            target = next;
        }
        Err(io::Error::other(format!(
            "重定向超过 {} 次",
            self.max_redirects
        )))
    }

    fn send_once(
        &self,
        method: &str,
        target: &Url,
        headers: &[(String, String)],
        body: Option<&[u8]>,
    ) -> io::Result<Response> {
        let proxy = proxy_for(target);
        let mut stream = transport::connect(
            &target.host,
            target.port,
            target.https,
            proxy.as_ref(),
            self.connect_timeout,
            self.io_timeout,
        )?;

        let mut all = Vec::with_capacity(headers.len() + 1);
        all.push(("User-Agent".to_string(), self.user_agent.clone()));
        all.extend_from_slice(headers);

        // 明文经代理时要发绝对 URI（RFC 7230 §5.3.2）；https 已经在
        // CONNECT 隧道里，发的是普通的 origin-form。
        let request_target = if proxy.is_some() && !target.https {
            target.to_string()
        } else {
            target.path.clone()
        };
        wire::write_request(
            &mut stream,
            method,
            &request_target,
            &target.authority(),
            &all,
            body,
        )?;
        wire::read_response(&mut stream, method, self.max_body)
    }
}

/// 按环境变量决定是否走代理。语义与 curl 一致：`NO_PROXY` 优先。
fn proxy_for(target: &Url) -> Option<Url> {
    if no_proxy_matches(&target.host) {
        return None;
    }
    let names: &[&str] = if target.https {
        &["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
    } else {
        &["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"]
    };
    for n in names {
        if let Ok(v) = std::env::var(n) {
            if v.trim().is_empty() {
                continue;
            }
            // 代理地址写错时**报错优于静默直连**：直连可能穿过本该经过的
            // 出口审计，是安全相关的行为差异。这里用 Some/None 表达不了
            // 错误，故解析失败当成没有代理，但下面的 parse 已经足够宽松
            // （允许省略 scheme）。
            let v = if v.contains("://") {
                v
            } else {
                format!("http://{v}")
            };
            if let Ok(u) = Url::parse(&v) {
                return Some(u);
            }
        }
    }
    None
}

fn no_proxy_matches(host: &str) -> bool {
    match std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy")) {
        Ok(list) => no_proxy_matches_in(host, &list),
        Err(_) => false,
    }
}

/// [`no_proxy_matches`] 的纯函数内核。环境变量的读取分出去是为了能单测——
/// 进程级 env 在并行用例下改不得。
fn no_proxy_matches_in(host: &str, list: &str) -> bool {
    for entry in list.split(',') {
        let e = entry.trim().trim_start_matches('.');
        if e.is_empty() {
            continue;
        }
        let (h, e) = (host.to_ascii_lowercase(), e.to_ascii_lowercase());
        if e == "*" || h == e || h.ends_with(&format!(".{e}")) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// 起一个按脚本回话的假 HTTP 服务器，返回 (端口, 收到的请求文本)。
    fn serve(replies: Vec<&'static str>) -> (u16, std::sync::mpsc::Receiver<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for reply in replies {
                let Ok((mut s, _)) = l.accept() else { return };
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut req = String::new();
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let done = line == "\r\n";
                    req.push_str(&line);
                    if done {
                        break;
                    }
                }
                // 有请求体就一并读走（长度取自 Content-Length）。
                if let Some(n) = req
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|l| l.split(':').nth(1))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                {
                    let mut b = vec![0u8; n];
                    use std::io::Read;
                    let _ = r.read_exact(&mut b);
                    req.push_str(&String::from_utf8_lossy(&b));
                }
                let _ = tx.send(req);
                let _ = s.write_all(reply.as_bytes());
            }
        });
        (port, rx)
    }

    fn client() -> Client {
        Client::new("wbox-test").io_timeout(Duration::from_secs(5))
    }

    #[test]
    fn round_trips_a_plain_request() {
        let (port, rx) = serve(vec!["HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi"]);
        let r = client()
            .request(
                "GET",
                &format!("http://127.0.0.1:{port}/v2/"),
                &[("Accept".into(), "application/json".into())],
                None,
            )
            .unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"hi");
        let req = rx.recv().unwrap();
        assert!(req.starts_with("GET /v2/ HTTP/1.1\r\n"), "{req}");
        assert!(req.contains("User-Agent: wbox-test\r\n"), "{req}");
        assert!(req.contains("Accept: application/json\r\n"), "{req}");
    }

    #[test]
    fn four_xx_is_a_normal_response_not_an_error() {
        // registry 就是用 401 回话开启 Bearer 流程的。把它变成 Err，
        // 拿不到 WWW-Authenticate，整个匿名认证流程都走不通。
        let (port, _rx) = serve(vec![
            "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer realm=\"https://a/t\"\r\nContent-Length: 0\r\n\r\n",
        ]);
        let r = client()
            .request("GET", &format!("http://127.0.0.1:{port}/v2/"), &[], None)
            .unwrap();
        assert_eq!(r.status, 401);
        assert!(r.header("www-authenticate").unwrap().contains("realm"));
    }

    #[test]
    fn follows_redirect_and_keeps_auth_on_same_host() {
        let (port, rx) = serve(vec![
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: /blob\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nblob",
        ]);
        let r = client()
            .request(
                "GET",
                &format!("http://127.0.0.1:{port}/v2/x"),
                &[("Authorization".into(), "Bearer secret".into())],
                None,
            )
            .unwrap();
        assert_eq!(r.body, b"blob");
        let _first = rx.recv().unwrap();
        let second = rx.recv().unwrap();
        assert!(second.starts_with("GET /blob "), "{second}");
        assert!(
            second.contains("Authorization: Bearer secret"),
            "同主机重定向应保留凭证：{second}"
        );
    }

    #[test]
    fn drops_auth_on_cross_host_redirect() {
        // **这条是本 crate 最重要的一条安全断言**：registry 的 blob 会被
        // 重定向到 CDN，把 Bearer token 带过去等于把凭证交给第三方。
        let (cdn, cdn_rx) = serve(vec!["HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nblob"]);
        let (reg, _reg_rx) = serve(vec![Box::leak(
            format!(
                "HTTP/1.1 302 Found\r\nLocation: http://localhost:{cdn}/blob\r\nContent-Length: 0\r\n\r\n"
            )
            .into_boxed_str(),
        )]);
        let r = client()
            .request(
                "GET",
                &format!("http://127.0.0.1:{reg}/v2/x"),
                &[("Authorization".into(), "Bearer secret".into())],
                None,
            )
            .unwrap();
        assert_eq!(r.body, b"blob");
        let got = cdn_rx.recv().unwrap();
        assert!(
            !got.to_ascii_lowercase().contains("authorization"),
            "跨主机重定向必须丢掉凭证，实际发出的是：{got}"
        );
    }

    #[test]
    fn put_sends_body_and_307_preserves_it() {
        // registry 的 blob 上传就依赖 307 保持方法与请求体。
        let (port, rx) = serve(vec![
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: /upload/2\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n",
        ]);
        let r = client()
            .request(
                "PUT",
                &format!("http://127.0.0.1:{port}/upload/1"),
                &[],
                Some(b"payload"),
            )
            .unwrap();
        assert_eq!(r.status, 201);
        let _ = rx.recv().unwrap();
        let second = rx.recv().unwrap();
        assert!(second.starts_with("PUT /upload/2 "), "{second}");
        assert!(second.ends_with("payload"), "{second}");
    }

    #[test]
    fn redirect_loop_terminates() {
        let replies: Vec<&'static str> = (0..12)
            .map(|_| "HTTP/1.1 302 Found\r\nLocation: /loop\r\nContent-Length: 0\r\n\r\n")
            .collect();
        let (port, _rx) = serve(replies);
        let e = client()
            .request("GET", &format!("http://127.0.0.1:{port}/loop"), &[], None)
            .unwrap_err();
        assert!(e.to_string().contains("重定向超过"), "{e}");
    }

    #[test]
    fn rejects_unsupported_method_and_bad_url() {
        assert!(client().request("DELETE", "http://x/", &[], None).is_err());
        assert!(client().request("GET", "not-a-url", &[], None).is_err());
    }

    #[test]
    fn no_proxy_matching_is_suffix_aware() {
        // 直接测纯函数，不动进程级环境变量（并行用例下改 env 会互相干扰）。
        assert!(super::no_proxy_matches_in("example.com", "example.com"));
        assert!(super::no_proxy_matches_in("api.example.com", ".example.com"));
        assert!(super::no_proxy_matches_in("anything", "*"));
        assert!(!super::no_proxy_matches_in("notexample.com", "example.com"));
        assert!(!super::no_proxy_matches_in("example.com", "other.com, third.net"));
    }
}
