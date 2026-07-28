//! HTTP/1.1 报文的编解码：请求行/头部的写出，状态行/头部/响应体的读入。
//!
//! **响应体这一侧面对的是网络输入**，所以三条上界都是硬的：头部总量、
//! 头部条数、响应体字节数。registry 的层可以是几 GiB，但"没有上限"和
//! "上限很大"是两回事——前者意味着一个恶意或故障的对端能把 wbox 撑爆。

use std::io::{self, BufRead, BufReader, Read, Write};

/// 单个响应头部块的上限（字节）。
const MAX_HEADER_BYTES: usize = 256 * 1024;
/// 头部条数上限。
const MAX_HEADERS: usize = 200;

/// 一次 HTTP 响应。
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    /// 头部名一律小写，方便调用方直接比对。
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// 写出请求行与头部。调用方负责随后写 body。
pub fn write_request(
    w: &mut impl Write,
    method: &str,
    target: &str,
    host: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> io::Result<()> {
    let mut req = Vec::with_capacity(256);
    write!(req, "{method} {target} HTTP/1.1\r\n")?;
    write!(req, "Host: {host}\r\n")?;
    // 不做连接复用：pull 一次也就几十个请求，而池化要维护"连接是否还健康"
    // 的状态，收益不抵复杂度。
    write!(req, "Connection: close\r\n")?;
    // 不声明 Accept-Encoding：层本身已经是 gzip，再套一层传输压缩没有意义，
    // 而且会让"响应体就是层字节"这条不变式多一个例外。
    write!(req, "Accept-Encoding: identity\r\n")?;
    for (k, v) in headers {
        if k.contains(['\r', '\n', ':']) || v.contains(['\r', '\n']) {
            // 头部注入：把攻击者控制的字符串原样拼进报文会被拆成额外的请求。
            return Err(bad(format!("非法头部：{k}: {v}")));
        }
        write!(req, "{k}: {v}\r\n")?;
    }
    if let Some(b) = body {
        write!(req, "Content-Length: {}\r\n", b.len())?;
    } else if matches!(method, "POST" | "PUT") {
        // 没有体的 POST/PUT 也要显式写 0，否则某些 registry 会一直等着读。
        write!(req, "Content-Length: 0\r\n")?;
    }
    req.extend_from_slice(b"\r\n");
    if let Some(b) = body {
        req.extend_from_slice(b);
    }
    w.write_all(&req)?;
    w.flush()
}

/// 读入状态行 + 头部 + 响应体。
pub fn read_response(stream: impl Read, method: &str, max_body: u64) -> io::Result<Response> {
    let mut r = BufReader::new(stream);

    // 1xx 是中间响应（如 100 Continue），要跳过后继续读真正的那一个。
    let (status, headers) = loop {
        let (status, headers) = read_head(&mut r)?;
        if !(100..200).contains(&status) {
            break (status, headers);
        }
    };

    let get = |name: &str| -> Option<&str> {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };

    // 这几种响应按定义没有 body，即便带了 Content-Length 也不能去读——
    // 读了会挂住（HEAD 尤其典型：registry 用它判断 blob 是否已存在）。
    let bodyless = method == "HEAD" || status == 204 || status == 304;
    let body = if bodyless {
        Vec::new()
    } else if get("transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false)
    {
        read_chunked(&mut r, max_body)?
    } else if let Some(len) = get("content-length") {
        let len: u64 = len
            .trim()
            .parse()
            .map_err(|_| bad(format!("Content-Length 非法：{len}")))?;
        if len > max_body {
            return Err(bad(format!("响应体 {len} 字节超过上限 {max_body}")));
        }
        let mut buf = vec![0u8; len as usize];
        r.read_exact(&mut buf)?;
        buf
    } else {
        // 既无 Content-Length 也非 chunked：读到连接关闭为止（HTTP/1.0 风格）。
        let mut buf = Vec::new();
        r.take(max_body + 1).read_to_end(&mut buf)?;
        if buf.len() as u64 > max_body {
            return Err(bad(format!("响应体超过上限 {max_body}")));
        }
        buf
    };

    Ok(Response {
        status,
        headers,
        body,
    })
}

fn read_head(r: &mut impl BufRead) -> io::Result<(u16, Vec<(String, String)>)> {
    let mut total = 0usize;
    let mut line = String::new();
    read_line(r, &mut line, &mut total)?;
    let status = parse_status_line(&line)?;

    let mut headers = Vec::new();
    loop {
        line.clear();
        read_line(r, &mut line, &mut total)?;
        let t = line.trim_end_matches(['\r', '\n']);
        if t.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Err(bad("响应头条数超过上限"));
        }
        let (k, v) = t
            .split_once(':')
            .ok_or_else(|| bad(format!("响应头缺少冒号：{t}")))?;
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
    }
    Ok((status, headers))
}

fn read_line(r: &mut impl BufRead, out: &mut String, total: &mut usize) -> io::Result<()> {
    let n = r.read_line(out)?;
    if n == 0 {
        return Err(bad("连接在读完响应头前就关闭了"));
    }
    *total += n;
    if *total > MAX_HEADER_BYTES {
        return Err(bad("响应头总量超过上限"));
    }
    Ok(())
}

fn parse_status_line(line: &str) -> io::Result<u16> {
    let mut parts = line.trim_end().splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") {
        return Err(bad(format!("不是 HTTP/1.x 响应：{line:?}")));
    }
    parts
        .next()
        .and_then(|c| c.parse::<u16>().ok())
        .filter(|c| (100..600).contains(c))
        .ok_or_else(|| bad(format!("状态码非法：{line:?}")))
}

fn read_chunked(r: &mut impl BufRead, max_body: u64) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Err(bad("分块传输在结束前断开"));
        }
        // 分块扩展（`;` 之后的部分）忽略。
        let size_text = line.trim_end().split(';').next().unwrap_or("").trim();
        let size = u64::from_str_radix(size_text, 16)
            .map_err(|_| bad(format!("分块长度非法：{size_text:?}")))?;
        if size == 0 {
            // trailer 段：读到空行为止。
            loop {
                let mut t = String::new();
                if r.read_line(&mut t)? == 0 {
                    break;
                }
                if t.trim_end().is_empty() {
                    break;
                }
            }
            return Ok(out);
        }
        if out.len() as u64 + size > max_body {
            return Err(bad(format!("响应体超过上限 {max_body}")));
        }
        let start = out.len();
        out.resize(start + size as usize, 0);
        r.read_exact(&mut out[start..])?;
        // 每块后面跟一个 CRLF。
        let mut crlf = [0u8; 2];
        r.read_exact(&mut crlf)?;
        if &crlf != b"\r\n" {
            return Err(bad("分块后缺少 CRLF"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(raw: &str, method: &str) -> io::Result<Response> {
        read_response(raw.as_bytes(), method, 1 << 20)
    }

    #[test]
    fn reads_content_length_body() {
        let r = resp(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 5\r\n\r\nhello",
            "GET",
        )
        .unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"hello");
        assert_eq!(r.header("content-type"), Some("application/json"));
    }

    #[test]
    fn header_names_are_lowercased() {
        // 调用方直接按小写名比对（WWW-Authenticate、Location 都是这么取的）。
        let r = resp(
            "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer realm=\"x\"\r\nContent-Length: 0\r\n\r\n",
            "GET",
        )
        .unwrap();
        assert_eq!(r.header("www-authenticate"), Some("Bearer realm=\"x\""));
    }

    #[test]
    fn reads_chunked_body() {
        let r = resp(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\n\r\n",
            "GET",
        )
        .unwrap();
        assert_eq!(r.body, b"hello world");
    }

    #[test]
    fn head_and_204_have_no_body_even_with_content_length() {
        // registry 用 HEAD 判断 blob 是否已存在。去读它声明的 Content-Length
        // 会一直挂住——这是最容易踩、且表现为"卡住"而不是报错的一个坑。
        let r = resp("HTTP/1.1 200 OK\r\nContent-Length: 999\r\n\r\n", "HEAD").unwrap();
        assert_eq!(r.status, 200);
        assert!(r.body.is_empty());
        let r = resp(
            "HTTP/1.1 204 No Content\r\nContent-Length: 5\r\n\r\n",
            "GET",
        )
        .unwrap();
        assert!(r.body.is_empty());
    }

    #[test]
    fn skips_informational_responses() {
        let r = resp(
            "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\nok",
            "PUT",
        )
        .unwrap();
        assert_eq!(r.status, 201);
        assert_eq!(r.body, b"ok");
    }

    #[test]
    fn reads_until_close_when_length_unknown() {
        let r = resp("HTTP/1.1 200 OK\r\n\r\nstreamed", "GET").unwrap();
        assert_eq!(r.body, b"streamed");
    }

    #[test]
    fn enforces_body_limit() {
        let raw = "HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n";
        assert!(read_response(raw.as_bytes(), "GET", 10).is_err());
        let chunked = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n64\r\n";
        assert!(read_response(chunked.as_bytes(), "GET", 10).is_err());
    }

    #[test]
    fn rejects_malformed_responses() {
        for (raw, method) in [
            ("", "GET"),
            ("NOT HTTP\r\n\r\n", "GET"),
            ("HTTP/1.1 999 X\r\n\r\n", "GET"),
            ("HTTP/1.1 200 OK\r\nbadheader\r\n\r\n", "GET"),
            ("HTTP/1.1 200 OK\r\nContent-Length: abc\r\n\r\n", "GET"),
            (
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
                "GET",
            ),
        ] {
            assert!(resp(raw, method).is_err(), "应当拒绝 {raw:?}");
        }
    }

    #[test]
    fn rejects_header_injection() {
        let mut out = Vec::new();
        let headers = vec![("X-Evil".to_string(), "a\r\nHost: attacker".to_string())];
        assert!(write_request(&mut out, "GET", "/", "h", &headers, None).is_err());
    }

    #[test]
    fn writes_well_formed_request() {
        let mut out = Vec::new();
        let headers = vec![("Authorization".to_string(), "Bearer t".to_string())];
        write_request(&mut out, "PUT", "/v2/x", "reg:5000", &headers, Some(b"ab")).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("PUT /v2/x HTTP/1.1\r\n"));
        assert!(text.contains("Host: reg:5000\r\n"));
        assert!(text.contains("Content-Length: 2\r\n"));
        assert!(text.ends_with("\r\nab"));

        // 没有体的 PUT 也要写 Content-Length: 0，否则对端会一直等着读。
        let mut out = Vec::new();
        write_request(&mut out, "PUT", "/", "h", &[], None).unwrap();
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("Content-Length: 0"));
    }
}
