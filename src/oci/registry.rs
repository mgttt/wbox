//! OCI Distribution Spec v2 HTTP 客户端（匿名 Bearer token 流程）。
//!
//! 流程：
//! 1. 请求 `/v2/<repo>/manifests/<ref>`，Accept 同时携带 manifest 与 manifest list 类型；
//! 2. 若 401，解析 `WWW-Authenticate: Bearer realm=...,service=...,scope=...`，
//!    向 realm 匿名请求 token（GET，query 带 service/scope），缓存后重试；
//! 3. blob 拉取同理（`/v2/<repo>/blobs/<digest>`）。
//!
//! 依赖 ureq（阻塞式，rustls），不引入 tokio。

use crate::error::{ErrKind, KindExt, WboxError};
use anyhow::Context;
use std::io::Read;

/// manifest / manifest list 的 Accept 集合（OCI + Docker 两种 media type）。
const ACCEPT_MANIFEST: &str = concat!(
    "application/vnd.oci.image.manifest.v1+json,",
    "application/vnd.oci.image.index.v1+json,",
    "application/vnd.docker.distribution.manifest.v2+json,",
    "application/vnd.docker.distribution.manifest.list.v2+json"
);

/// 一次 registry 会话：保存 host、HTTP agent 与（可选的）匿名 Bearer token。
pub struct RegistryClient {
    registry: String,
    agent: ureq::Agent,
    token: std::cell::RefCell<Option<String>>,
}

/// HTTP GET 结果（状态码 + 响应头 + body 字节）。
struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl RegistryClient {
    /// 构造指定 registry 主机的客户端。
    pub fn new(registry: &str) -> Self {
        // ureq 的 native-tls 后端需显式注入 connector（rustls 才有默认构造）。
        // native-tls 在 Windows 走 schannel、Linux 走系统 OpenSSL，均为纯 FFI 无 C 编译。
        let tls = native_tls::TlsConnector::new().expect("初始化系统 TLS 失败");
        let agent = ureq::AgentBuilder::new()
            .tls_connector(std::sync::Arc::new(tls))
            .timeout_connect(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(300)) // 大层下载放宽
            .user_agent(concat!("wbox/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            registry: registry.to_string(),
            agent,
            token: std::cell::RefCell::new(None),
        }
    }

    /// 底层 GET；ureq 对 4xx/5xx 返回 Err(Response)，这里统一展开为 HttpResponse。
    fn raw_get(&self, url: &str, accept: Option<&str>) -> crate::error::Result<HttpResponse> {
        let mut req = self.agent.get(url);
        if let Some(a) = accept {
            req = req.set("Accept", a);
        }
        if let Some(t) = self.token.borrow().as_ref() {
            req = req.set("Authorization", &format!("Bearer {}", t));
        } else if let Some(basic) = basic_auth_from_env() {
            // 私有 registry 可选基本认证（WBOX_REGISTRY_USER/PASS），base64(user:pass)
            req = req.set("Authorization", &basic);
        }
        let resp = match req.call() {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r, // 非 2xx：照常读取，交由上层判断
            Err(ureq::Error::Transport(t)) => {
                return Err(WboxError::new(
                    ErrKind::Registry,
                    anyhow::anyhow!(t).context(format!("网络请求失败: {}", url)),
                ));
            }
        };
        let status = resp.status();
        let headers = resp
            .headers_names()
            .into_iter()
            .map(|n| {
                let v = resp.header(&n).unwrap_or("").to_string();
                (n.to_ascii_lowercase(), v)
            })
            .collect();
        let mut body = Vec::new();
        resp.into_reader()
            .read_to_end(&mut body)
            .context("读取响应体失败")
            .ctx(ErrKind::Registry)?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }

    /// 从 401 响应的 WWW-Authenticate 头解析 Bearer 参数并匿名取 token。
    /// 形如：`Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/ubuntu:pull"`
    fn authenticate(&self, resp: &HttpResponse) -> crate::error::Result<()> {
        let header = resp
            .headers
            .iter()
            .find(|(k, _)| k == "www-authenticate")
            .map(|(_, v)| v.clone())
            .ok_or_else(|| WboxError::registry("401 响应缺少 WWW-Authenticate 头"))?;
        if !header.to_ascii_lowercase().starts_with("bearer") {
            return Err(WboxError::registry(format!(
                "不支持的认证方式（非 Bearer）：{}",
                header
            )));
        }
        let params = parse_auth_params(&header);
        let realm = params
            .get("realm")
            .ok_or_else(|| WboxError::registry("WWW-Authenticate 缺少 realm"))?;
        // realm 校验：仅允许 https，防止降级到明文 http 泄露/注入 token（L10）
        if !realm.to_ascii_lowercase().starts_with("https://") {
            return Err(WboxError::registry(format!(
                "认证 realm 非 https，拒绝请求 token：{}",
                realm
            )));
        }
        let service = params.get("service").cloned().unwrap_or_default();
        let scope = params.get("scope").cloned().unwrap_or_default();

        // 匿名 token 请求：GET realm?service=...&scope=...（不带 basic auth）
        let url = format!(
            "{}?service={}&scope={}",
            realm,
            url_encode(&service),
            url_encode(&scope)
        );
        let saved = self.token.borrow_mut().take(); // 避免带旧 token 请求 token 端点
        let resp = self.raw_get(&url, None);
        *self.token.borrow_mut() = saved; // 先恢复，成功后覆盖
        let resp = resp?;
        if resp.status != 200 {
            return Err(WboxError::registry(format!(
                "获取匿名 token 失败：HTTP {}（{}）",
                resp.status, realm
            )));
        }
        let v: serde_json::Value = serde_json::from_slice(&resp.body)
            .context("token 响应不是合法 JSON")
            .ctx(ErrKind::Registry)?;
        // Docker 返回 token 字段；部分实现用 access_token（OAuth2 风格）
        let token = v
            .get("token")
            .or_else(|| v.get("access_token"))
            .and_then(|t| t.as_str())
            .ok_or_else(|| WboxError::registry("token 响应缺少 token/access_token 字段"))?;
        *self.token.borrow_mut() = Some(token.to_string());
        Ok(())
    }

    /// 带自动认证的 GET：401 时走 Bearer 流程后重试一次。
    fn get_authed(&self, url: &str, accept: Option<&str>) -> crate::error::Result<HttpResponse> {
        let resp = self.raw_get(url, accept)?;
        if resp.status == 401 {
            self.authenticate(&resp)?;
            let resp = self.raw_get(url, accept)?;
            if resp.status == 401 {
                return Err(WboxError::registry(format!(
                    "认证后仍 401（匿名无权限？）：{}",
                    url
                )));
            }
            return Ok(resp);
        }
        Ok(resp)
    }

    /// 拉取 manifest（可能是单 manifest 或 manifest list/index）。
    /// 返回 (content-type, body, Docker-Content-Digest 响应头值)。
    pub fn get_manifest(
        &self,
        repo: &str,
        reference: &str,
    ) -> crate::error::Result<(String, Vec<u8>, Option<String>)> {
        let url = format!(
            "https://{}/v2/{}/manifests/{}",
            self.registry, repo, reference
        );
        let resp = self.get_authed(&url, Some(ACCEPT_MANIFEST))?;
        if resp.status != 200 {
            return Err(WboxError::registry(format!(
                "拉取 manifest 失败：HTTP {}（{}）\n{}",
                resp.status,
                url,
                String::from_utf8_lossy(&resp.body)
            )));
        }
        let ctype = resp
            .headers
            .iter()
            .find(|(k, _)| k == "content-type")
            .map(|(_, v)| v.split(';').next().unwrap_or("").trim().to_string())
            .unwrap_or_default();
        let digest = resp
            .headers
            .iter()
            .find(|(k, _)| k == "docker-content-digest")
            .map(|(_, v)| v.clone());
        Ok((ctype, resp.body, digest))
    }

    /// 拉取 blob（config 或 layer），返回原始字节。
    /// 对 transport error / 5xx 做 3 次指数退避重试（0.5s/1s/2s，L8）。
    pub fn get_blob(&self, repo: &str, digest: &str) -> crate::error::Result<Vec<u8>> {
        let url = format!("https://{}/v2/{}/blobs/{}", self.registry, repo, digest);
        let mut delay = std::time::Duration::from_millis(500);
        let mut last_err: Option<WboxError> = None;
        for attempt in 0..3 {
            match self.get_authed(&url, None) {
                Ok(resp) if resp.status == 200 => return Ok(resp.body),
                Ok(resp) if resp.status >= 500 => {
                    // 5xx：可重试
                    last_err = Some(WboxError::registry(format!(
                        "拉取 blob 失败：HTTP {}（{}）",
                        resp.status, url
                    )));
                }
                Ok(resp) => {
                    // 4xx 等：不可重试，直接报错
                    return Err(WboxError::registry(format!(
                        "拉取 blob 失败：HTTP {}（{}）",
                        resp.status, url
                    )));
                }
                Err(e) => last_err = Some(e), // transport error：可重试
            }
            if attempt < 2 {
                std::thread::sleep(delay);
                delay *= 2;
            }
        }
        Err(last_err.unwrap_or_else(|| WboxError::registry(format!("拉取 blob 失败：{}", url))))
    }
}

/// 解析 `Bearer realm="a",service="b",scope="c"` 形式的参数表。
fn parse_auth_params(header: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    // 去掉开头的 "Bearer "
    let s = header[header.find(' ').map(|i| i + 1).unwrap_or(0)..].trim();
    for part in s.split(',') {
        if let Some((k, v)) = part.split_once('=') {
            map.insert(
                k.trim().to_ascii_lowercase(),
                v.trim().trim_matches('"').to_string(),
            );
        }
    }
    map
}

/// 从环境变量构造可选的 Basic 认证头（用于私有 registry 的 token 端点）。
fn basic_auth_from_env() -> Option<String> {
    use base64::Engine;
    let u = std::env::var("WBOX_REGISTRY_USER").ok()?;
    let p = std::env::var("WBOX_REGISTRY_PASS").ok()?;
    let cred = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", u, p));
    Some(format!("Basic {}", cred))
}

/// 最小 URL query 编码（service/scope 里只有 `:`, `/` 等少数字符需要转义）。
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_params_parse() {
        let m = parse_auth_params(
            r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/ubuntu:pull""#,
        );
        assert_eq!(m.get("realm").unwrap(), "https://auth.docker.io/token");
        assert_eq!(m.get("service").unwrap(), "registry.docker.io");
        assert_eq!(
            m.get("scope").unwrap(),
            "repository:library/ubuntu:pull"
        );
    }

    // ---- L10：realm 仅允许 https ----

    #[test]
    fn realm_must_be_https() {
        let client = RegistryClient::new("example.com");
        let resp = HttpResponse {
            status: 401,
            headers: vec![(
                "www-authenticate".to_string(),
                r#"Bearer realm="http://evil.example/token",service="s",scope="x""#.to_string(),
            )],
            body: Vec::new(),
        };
        let e = client.authenticate(&resp).unwrap_err();
        assert!(
            e.to_string().contains("非 https"),
            "http realm 必须被拒绝：{}",
            e
        );
    }

    #[test]
    fn url_encode_scope() {
        assert_eq!(
            url_encode("repository:library/ubuntu:pull"),
            "repository%3Alibrary%2Fubuntu%3Apull"
        );
    }
}
