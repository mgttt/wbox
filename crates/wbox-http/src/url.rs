//! 最小 URL 解析：只认 `scheme://host[:port]/path[?query]`。
//!
//! 不做通用 URL 规范化（不解析用户名密码、不做百分号解码、不做相对路径
//! 合并）——registry 的 URL 都是我们自己拼的，或来自 `Location` 头，形态
//! 是有限的。**拒绝比猜好**：认不出来就报错，不要"尽力而为"地连到某个
//! 意想不到的主机上去，那在带凭证的场景里是安全问题。

use std::fmt;

/// 拆开的 URL。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    /// 含前导 `/`，带 query。
    pub path: String,
}

impl Url {
    pub fn parse(s: &str) -> Result<Url, String> {
        let (scheme, rest) = s
            .split_once("://")
            .ok_or_else(|| format!("URL 缺少 scheme：{s}"))?;
        let https = match scheme.to_ascii_lowercase().as_str() {
            "https" => true,
            "http" => false,
            other => return Err(format!("不支持的 URL scheme：{other}")),
        };
        let (authority, path) = match rest.find(['/', '?', '#']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        if authority.contains('@') {
            // URL 里内嵌凭证是个坑（会连带出现在日志与重定向里）。明确拒绝。
            return Err("URL 不得内嵌用户名/密码".to_string());
        }
        let (host, port) = split_host_port(authority, https)?;
        if host.is_empty() {
            return Err(format!("URL 缺少主机名：{s}"));
        }
        let path = if path.is_empty() {
            "/".to_string()
        } else {
            // 片段（`#...`）不发给服务器。
            path.split('#').next().unwrap_or("/").to_string()
        };
        Ok(Url {
            https,
            host,
            port,
            path,
        })
    }

    /// `host` 或 `host:port`（非默认端口时带端口），用于 `Host` 头与 SNI 判定。
    pub fn authority(&self) -> String {
        let default = if self.https { 443 } else { 80 };
        if self.port == default {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// 把 `Location` 里可能出现的相对形式解析成绝对 URL。
    pub fn join(&self, location: &str) -> Result<Url, String> {
        if location.contains("://") {
            return Url::parse(location);
        }
        let mut next = self.clone();
        if let Some(abs) = location.strip_prefix('/') {
            next.path = format!("/{abs}");
        } else {
            // 相对路径：替换掉当前路径的最后一段。
            let base = self.path.split('?').next().unwrap_or("/");
            let cut = base.rfind('/').map(|i| i + 1).unwrap_or(1);
            next.path = format!("{}{}", &base[..cut], location);
        }
        Ok(next)
    }
}

fn split_host_port(authority: &str, https: bool) -> Result<(String, u16), String> {
    let default = if https { 443 } else { 80 };
    // IPv6 字面量形如 [::1]:443。
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| format!("IPv6 地址未闭合：{authority}"))?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| format!("端口非法：{authority}"))?,
            None => default,
        };
        return Ok((host.to_ascii_lowercase(), port));
    }
    match authority.rsplit_once(':') {
        Some((h, p)) => Ok((
            h.to_ascii_lowercase(),
            p.parse().map_err(|_| format!("端口非法：{authority}"))?,
        )),
        None => Ok((authority.to_ascii_lowercase(), default)),
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}://{}{}",
            if self.https { "https" } else { "http" },
            self.authority(),
            self.path
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        let u =
            Url::parse("https://registry-1.docker.io/v2/library/ubuntu/manifests/latest").unwrap();
        assert!(u.https);
        assert_eq!(u.host, "registry-1.docker.io");
        assert_eq!(u.port, 443);
        assert_eq!(u.path, "/v2/library/ubuntu/manifests/latest");

        let u = Url::parse("http://127.0.0.1:5000/v2/").unwrap();
        assert!(!u.https);
        assert_eq!(u.port, 5000);
        assert_eq!(u.authority(), "127.0.0.1:5000");

        // 无路径、带 query、大写 host
        let u = Url::parse("https://Auth.Docker.IO?service=x").unwrap();
        assert_eq!(u.host, "auth.docker.io");
        assert_eq!(u.path, "?service=x");

        let u = Url::parse("https://[::1]:8443/v2/").unwrap();
        assert_eq!(u.host, "::1");
        assert_eq!(u.port, 8443);
    }

    #[test]
    fn rejects_rather_than_guesses() {
        // 认不出来就报错——带凭证的请求不能连到"猜出来"的主机上。
        for bad in [
            "registry.example.com/v2/",
            "ftp://example.com/x",
            "https://user:pass@example.com/",
            "https://example.com:notaport/",
        ] {
            assert!(Url::parse(bad).is_err(), "应当拒绝 {bad}");
        }
    }

    #[test]
    fn joins_locations() {
        let base = Url::parse("https://reg.example.com/v2/repo/blobs/uploads/abc?x=1").unwrap();
        assert_eq!(
            base.join("/v2/other").unwrap().to_string(),
            "https://reg.example.com/v2/other"
        );
        assert_eq!(
            base.join("def").unwrap().to_string(),
            "https://reg.example.com/v2/repo/blobs/uploads/def"
        );
        assert_eq!(
            base.join("https://cdn.example.net/x").unwrap().host,
            "cdn.example.net"
        );
    }
}
