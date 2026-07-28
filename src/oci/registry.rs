//! OCI Distribution Spec v2 HTTP 客户端（匿名 Bearer token 流程）。
//!
//! 流程：
//! 1. 请求 `/v2/<repo>/manifests/<ref>`，Accept 同时携带 manifest 与 manifest list 类型；
//! 2. 若 401，解析 `WWW-Authenticate: Bearer realm=...,service=...,scope=...`，
//!    向 realm 匿名请求 token（GET，query 带 service/scope），缓存后重试；
//! 3. blob 拉取同理（`/v2/<repo>/blobs/<digest>`）。
//!
//! HTTP 走自实现的 `wbox-http`（阻塞式，无 tokio）。跨主机重定向丢弃
//! `Authorization`、响应体上限、头部注入拒绝这几条都在那边，本文件只负责
//! registry 协议本身（Bearer 流程、凭证发放规则、digest 语义）。

use crate::error::{ErrKind, KindExt, WboxError};
use crate::fault::Context;

/// 单个 HTTP 响应体的上限。镜像层可以很大，但也不该无上限地吃内存。
const MAX_RESPONSE_BYTES: u64 = 8 << 30; // 8 GiB

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
    http: wbox_http::Client,
    token: std::cell::RefCell<Option<String>>,
}

/// HTTP 响应就用 `wbox-http` 的那一个类型。
///
/// 这里**不再包一层同形状的结构体**：两个只差名字的类型意味着每加一个字段
/// 都要记得同步两处，而漏改一处不会有任何东西提醒你。
type HttpResponse = wbox_http::Response;

impl RegistryClient {
    /// 构造指定 registry 主机的客户端。
    pub fn new(registry: &str) -> Self {
        let http = wbox_http::Client::new(concat!("wbox/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(15))
            // 大层下载放宽；这是**每次读写**的超时而不是整体超时。
            .io_timeout(std::time::Duration::from_secs(300))
            .max_body(MAX_RESPONSE_BYTES);
        Self {
            registry: registry.to_string(),
            http,
            token: std::cell::RefCell::new(None),
        }
    }

    /// 本 registry 的 URL scheme。默认 https；仅当 host 精确匹配
    /// `WBOX_INSECURE_REGISTRY` 时才降级 http（见 [`insecure_host_allowed`]）。
    pub(crate) fn scheme(&self) -> &'static str {
        if insecure_host_allowed(&self.registry) {
            "http"
        } else {
            "https"
        }
    }

    /// `<scheme>://<registry>/v2/<repo>` 前缀。
    fn v2(&self, repo: &str) -> String {
        format!("{}://{}/v2/{}", self.scheme(), self.registry, repo)
    }

    /// 底层 GET。4xx/5xx 也按正常响应返回（agent 配了
    /// `http_status_as_error(false)`），由调用方判断状态码。
    fn raw_get(&self, url: &str, accept: Option<&str>) -> crate::error::Result<HttpResponse> {
        self.raw_request("GET", url, accept, None, None)
    }

    /// 底层请求。GET/HEAD/POST/PUT 共用一条路径——认证头的附加规则（Bearer
    /// 优先、Basic 只发同 host）**只能有一处实现**，push 另写一份迟早会漂移
    /// 成"上传时把凭证发去了别处"。
    fn raw_request(
        &self,
        method: &str,
        url: &str,
        accept: Option<&str>,
        content_type: Option<&str>,
        body: Option<&[u8]>,
    ) -> crate::error::Result<HttpResponse> {
        // 先把要发的头收集好，再一次性交给 HTTP 客户端。
        let mut headers: Vec<(&str, String)> = Vec::new();
        if let Some(a) = accept {
            headers.push(("Accept", a.to_string()));
        }
        if let Some(c) = content_type {
            headers.push(("Content-Type", c.to_string()));
        }
        if let Some(t) = self.token.borrow().as_ref() {
            headers.push(("Authorization", format!("Bearer {}", t)));
        } else if url_host_matches(url, &self.registry) {
            // H3：Basic 凭证（WBOX_REGISTRY_USER/PASS）只发给与 registry
            // 同 host 的 URL——跨 host 的 realm/重定向不得携带凭证。
            // `url_host_matches` 同时要求 https，故明文（insecure 逃生口）
            // 走不到这里——凭证绝不上明文信道。
            if let Some(basic) = basic_auth_from_env() {
                // 私有 registry 可选基本认证，base64(user:pass)
                headers.push(("Authorization", basic));
            }
        }

        // 4xx/5xx 是**正常返回**而不是 Err——registry 就是用 401 回话开启
        // Bearer 流程的，把它变成错误会让整个匿名认证流程走不通。
        let headers: Vec<(String, String)> = headers
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let resp = self
            .http
            .request(method, url, &headers, body)
            .map_err(|e| {
                WboxError::new(
                    ErrKind::Registry,
                    crate::fail!(e).context(format!("网络请求失败: {}", url)),
                )
            })?;
        let (status, headers, body) = (resp.status, resp.headers, resp.body);
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
        // H3：realm 必须是 https 且与 registry 同 host（或显式允许列表），
        // 否则恶意 registry 可把 token 请求（乃至凭证）导向攻击者端点。
        if !realm_host_allowed(realm, &self.registry) {
            return Err(WboxError::registry(format!(
                "认证 realm host 与 registry '{}' 不同且不在允许列表，拒绝请求 token：{}",
                self.registry, realm
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
        let v: wbox_codec::json::Value = wbox_codec::json::from_slice(&resp.body)
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
        self.request_authed("GET", url, accept, None, None)
    }

    /// 带自动认证的任意方法请求。push 的三步全走这里：401 的
    /// `WWW-Authenticate` 里 scope 自带 `push,pull`，[`Self::authenticate`]
    /// 照常解析，不需要为 push 另写一套 token 流程。
    fn request_authed(
        &self,
        method: &str,
        url: &str,
        accept: Option<&str>,
        content_type: Option<&str>,
        body: Option<&[u8]>,
    ) -> crate::error::Result<HttpResponse> {
        let resp = self.raw_request(method, url, accept, content_type, body)?;
        if resp.status == 401 {
            self.authenticate(&resp)?;
            let resp = self.raw_request(method, url, accept, content_type, body)?;
            if resp.status == 401 {
                return Err(WboxError::registry(format!(
                    "认证后仍 401（无权限？）：{} {}",
                    method, url
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
        let url = format!("{}/manifests/{}", self.v2(repo), reference);
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
        let url = format!("{}/blobs/{}", self.v2(repo), digest);
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

    // ---- push（OCI Distribution 上传，PRD F9.13）----

    /// blob 是否已在 registry 上。先问一句能省掉整层的上传，
    /// 对反复推同一基础镜像是实打实的节省。
    pub fn blob_exists(&self, repo: &str, digest: &str) -> crate::error::Result<bool> {
        let url = format!("{}/blobs/{}", self.v2(repo), digest);
        let resp = self.request_authed("HEAD", &url, None, None, None)?;
        Ok(resp.status == 200)
    }

    /// 上传一个 blob（单体方式）：`POST` 拿 upload location → `PUT ?digest=`。
    ///
    /// 走单体而非分块：层已经整份在内存里（rootfs 是本地打包出来的），
    /// 分块只会引入断点续传的状态机而不带来任何好处。
    ///
    /// 返回 `false` 表示 registry 已有该 blob、这次**没有上传**。调用方据此
    /// 如实告诉用户跳过了哪些层——分层复用省下的流量正体现在这里，
    /// 一律打印"上传"会把这个收益说没了。
    pub fn push_blob(&self, repo: &str, digest: &str, data: &[u8]) -> crate::error::Result<bool> {
        if self.blob_exists(repo, digest)? {
            return Ok(false);
        }
        let start = format!("{}/blobs/uploads/", self.v2(repo));
        let resp = self.request_authed("POST", &start, None, None, Some(&[]))?;
        // 202 Accepted 是常规；个别实现直接 201 完成（少见但合法）
        if resp.status == 201 {
            return Ok(true);
        }
        if resp.status != 202 {
            return Err(WboxError::registry(format!(
                "发起 blob 上传失败：HTTP {}（{}）\n{}",
                resp.status,
                start,
                String::from_utf8_lossy(&resp.body)
            )));
        }
        let location = resp
            .headers
            .iter()
            .find(|(k, _)| k == "location")
            .map(|(_, v)| v.clone())
            .ok_or_else(|| WboxError::registry("blob 上传响应缺少 Location 头"))?;
        let url = self.absolutize(&location)?;
        // Location 可能已带 query（多数实现会带 upload uuid），拼接符要跟着变
        let sep = if url.contains('?') { '&' } else { '?' };
        let put = format!("{}{}digest={}", url, sep, url_encode(digest));
        let resp = self.request_authed(
            "PUT",
            &put,
            None,
            Some("application/octet-stream"),
            Some(data),
        )?;
        if resp.status != 201 && resp.status != 204 {
            return Err(WboxError::registry(format!(
                "上传 blob 失败：HTTP {}（{}）\n{}",
                resp.status,
                put,
                String::from_utf8_lossy(&resp.body)
            )));
        }
        Ok(true)
    }

    /// 推送 manifest，返回 registry 侧记录的 digest（若响应头带）。
    pub fn put_manifest(
        &self,
        repo: &str,
        reference: &str,
        content_type: &str,
        body: &[u8],
    ) -> crate::error::Result<Option<String>> {
        let url = format!("{}/manifests/{}", self.v2(repo), reference);
        let resp =
            self.request_authed("PUT", &url, None, Some(content_type), Some(body))?;
        if resp.status != 201 && resp.status != 200 {
            return Err(WboxError::registry(format!(
                "推送 manifest 失败：HTTP {}（{}）\n{}",
                resp.status,
                url,
                String::from_utf8_lossy(&resp.body)
            )));
        }
        Ok(resp
            .headers
            .iter()
            .find(|(k, _)| k == "docker-content-digest")
            .map(|(_, v)| v.clone()))
    }

    /// 把 `Location` 补成绝对 URL，并**校验它没被导去别的 host**。
    ///
    /// registry 可以返回相对路径（合法），也可以返回绝对 URL。后者若指向
    /// 其他主机，就是把待上传内容（可能含私有镜像）导向第三方——必须拒绝，
    /// 与 realm 的同 host 约束（H3）是同一条规矩。
    fn absolutize(&self, location: &str) -> crate::error::Result<String> {
        if !location.contains("://") {
            let sep = if location.starts_with('/') { "" } else { "/" };
            return Ok(format!(
                "{}://{}{}{}",
                self.scheme(),
                self.registry,
                sep,
                location
            ));
        }
        if url_host(location).as_deref() != Some(&self.registry.to_ascii_lowercase()) {
            return Err(WboxError::registry(format!(
                "blob 上传 Location 指向其他主机，拒绝上传：{}",
                location
            )));
        }
        Ok(location.to_string())
    }
}

/// 是否允许对该 host 走明文 http。
///
/// 默认永远不允许。只有 `WBOX_INSECURE_REGISTRY` 精确等于该 host 时才降级——
/// 这是为**本地 registry / 门禁 stub** 留的逃生口（docker 的
/// `--insecure-registry` 同理）。两条硬约束：必须精确匹配（不做前缀/通配），
/// 且明文信道上**绝不发送凭证**（`url_host_matches` 要求 https，Basic 分支
/// 因此走不到）。
pub(crate) fn insecure_host_allowed(registry: &str) -> bool {
    match std::env::var("WBOX_INSECURE_REGISTRY") {
        Ok(v) => !v.is_empty() && v.eq_ignore_ascii_case(registry),
        Err(_) => false,
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

/// 显式允许的跨 host realm 主机（H3）：Docker Hub 的 token 端点在
/// auth.docker.io 而非 registry-1.docker.io，属合法已知端点。
const ALLOWED_REALM_HOSTS: &[&str] = &["auth.docker.io"];

/// 提取 URL 的 host 部分（可含端口），去掉 scheme 与 path；无 scheme 返回 None。
fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r)?;
    let host = rest.split('/').next()?;
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// URL 是否为 https 且 host 与 registry 主机一致（大小写不敏感，端口参与比较）。
fn url_host_matches(url: &str, registry: &str) -> bool {
    url.to_ascii_lowercase().starts_with("https://")
        && url_host(url).as_deref() == Some(&registry.to_ascii_lowercase())
}

/// realm 是否允许请求 token：https 且 host 与 registry 相同，或 host 在
/// 显式允许列表（如 Docker Hub 的 auth.docker.io）。
fn realm_host_allowed(realm: &str, registry: &str) -> bool {
    match url_host(realm) {
        Some(h) => {
            h == registry.to_ascii_lowercase()
                || ALLOWED_REALM_HOSTS.iter().any(|a| h == *a)
        }
        None => false,
    }
}

/// 从环境变量构造可选的 Basic 认证头（用于私有 registry 的 token 端点）。
fn basic_auth_from_env() -> Option<String> {
    let u = std::env::var("WBOX_REGISTRY_USER").ok()?;
    let p = std::env::var("WBOX_REGISTRY_PASS").ok()?;
    let cred = wbox_codec::base64::encode(format!("{}:{}", u, p).as_bytes());
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

    // ---- H3：realm host 必须同 registry（或显式允许列表）----

    #[test]
    fn realm_foreign_host_rejected() {
        let client = RegistryClient::new("registry.example.com");
        let resp = HttpResponse {
            status: 401,
            headers: vec![(
                "www-authenticate".to_string(),
                // https 但指向攻击者 host：必须拒绝（凭证/token 不得外发）
                r#"Bearer realm="https://evil.example/token",service="s",scope="x""#.to_string(),
            )],
            body: Vec::new(),
        };
        let e = client.authenticate(&resp).unwrap_err();
        assert!(
            e.to_string().contains("不在允许列表"),
            "伪造 realm 必须被拒绝：{}",
            e
        );
    }

    #[test]
    fn realm_host_allowed_rules() {
        // 同 host（大小写不敏感）
        assert!(realm_host_allowed(
            "https://Registry.Example.COM/token",
            "registry.example.com"
        ));
        // 带端口需端口一致
        assert!(realm_host_allowed(
            "https://localhost:5000/auth",
            "localhost:5000"
        ));
        assert!(!realm_host_allowed("https://localhost:6000/auth", "localhost:5000"));
        // 允许列表：Docker Hub 的 auth.docker.io
        assert!(realm_host_allowed(
            "https://auth.docker.io/token",
            "registry-1.docker.io"
        ));
        // 不同 host / 无 scheme / 相似域名欺骗均拒绝
        assert!(!realm_host_allowed("https://evil.example/token", "registry.example.com"));
        assert!(!realm_host_allowed(
            "https://registry.example.com.evil.example/token",
            "registry.example.com"
        ));
        assert!(!realm_host_allowed("//evil.example/token", "registry.example.com"));
    }

    #[test]
    fn basic_auth_scoped_to_registry_host() {
        assert!(url_host_matches(
            "https://registry.example.com/v2/ns/app/manifests/1",
            "registry.example.com"
        ));
        assert!(!url_host_matches(
            "https://evil.example/token",
            "registry.example.com"
        ));
        assert!(!url_host_matches(
            "http://registry.example.com/v2/x", // 非 https 也不发凭证
            "registry.example.com"
        ));
    }

    #[test]
    fn url_encode_scope() {
        assert_eq!(
            url_encode("repository:library/ubuntu:pull"),
            "repository%3Alibrary%2Fubuntu%3Apull"
        );
    }

    // ---- 表驱动：realm_host_allowed / url_host_matches 全组合 ----

    #[test]
    fn realm_host_allowed_table() {
        let cases: &[(&str, &str, bool)] = &[
            // 同 host
            ("https://registry.example.com/token", "registry.example.com", true),
            // 同 host 大小写不敏感
            ("https://REGISTRY.EXAMPLE.COM/token", "registry.example.com", true),
            // 同 host 带端口一致
            ("https://localhost:5000/auth", "localhost:5000", true),
            // 端口不一致
            ("https://localhost:5001/auth", "localhost:5000", false),
            // 允许列表（docker hub 已知 token 端点）
            ("https://auth.docker.io/token", "registry-1.docker.io", true),
            ("https://auth.docker.io/token", "quay.io", true), // 列表与 registry 无关
            // 跨 host
            ("https://evil.example/token", "registry.example.com", false),
            // 相似域名欺骗
            ("https://registry.example.com.evil.io/t", "registry.example.com", false),
            ("https://evil-registry.example.com/t", "registry.example.com", false),
            // 无 scheme / 空 host
            ("//registry.example.com/token", "registry.example.com", false),
            ("registry.example.com/token", "registry.example.com", false),
            ("https:///token", "registry.example.com", false),
            // 注意：http 同 host 在 url_host 层面匹配；https 强制由 authenticate
            // 的 scheme 检查单独保证（见 realm_must_be_https）
            ("http://registry.example.com/token", "registry.example.com", true),
        ];
        for (realm, registry, want) in cases {
            assert_eq!(realm_host_allowed(realm, registry), *want,
                "realm={} registry={}", realm, registry);
        }
    }

    #[test]
    fn url_host_matches_table() {
        let cases: &[(&str, &str, bool)] = &[
            // Basic 凭证作用域：仅 https + 同 host
            ("https://reg.example/v2/x", "reg.example", true),
            ("https://REG.EXAMPLE/v2/x", "reg.example", true),
            ("http://reg.example/v2/x", "reg.example", false), // http 拒绝
            ("https://other.example/v2/x", "reg.example", false), // 跨 host
            ("https://reg.example:444/v2/x", "reg.example", false), // 端口参与比较
            ("https://reg.example:444/v2/x", "reg.example:444", true),
            ("https://reg.example.evil/v2/x", "reg.example", false),
        ];
        for (url, registry, want) in cases {
            assert_eq!(url_host_matches(url, registry), *want,
                "url={} registry={}", url, registry);
        }
    }

    // ---- authenticate 的预网络失败路径（不触发 HTTP） ----

    fn resp401(www_auth: Option<&str>) -> HttpResponse {
        HttpResponse {
            status: 401,
            headers: www_auth
                .map(|v| vec![("www-authenticate".to_string(), v.to_string())])
                .unwrap_or_default(),
            body: Vec::new(),
        }
    }

    #[test]
    fn authenticate_missing_www_authenticate_header() {
        let client = RegistryClient::new("example.com");
        let e = client.authenticate(&resp401(None)).unwrap_err();
        assert!(e.to_string().contains("WWW-Authenticate"), "{}", e);
    }

    #[test]
    fn authenticate_rejects_non_bearer_scheme() {
        let client = RegistryClient::new("example.com");
        let e = client
            .authenticate(&resp401(Some(r#"Basic realm="https://example.com/""#)))
            .unwrap_err();
        assert!(e.to_string().contains("不支持的认证方式"), "{}", e);
    }

    #[test]
    fn authenticate_rejects_missing_realm() {
        let client = RegistryClient::new("example.com");
        let e = client
            .authenticate(&resp401(Some(r#"Bearer service="s",scope="x""#)))
            .unwrap_err();
        assert!(e.to_string().contains("缺少 realm"), "{}", e);
    }

    #[test]
    fn authenticate_http_realm_rejected_before_host_check() {
        // http scheme 即使同 host 也先被 L10 检查拒绝
        let client = RegistryClient::new("example.com");
        let e = client
            .authenticate(&resp401(Some(r#"Bearer realm="http://example.com/token""#)))
            .unwrap_err();
        assert!(e.to_string().contains("非 https"), "{}", e);
    }

    #[test]
    fn authenticate_https_prefix_case_insensitive() {
        // HTTPS:// 大写 scheme 也通过 https 检查，随后被 host 校验拒绝
        // （说明 scheme 检查大小写不敏感，不触发网络）
        let client = RegistryClient::new("example.com");
        let e = client
            .authenticate(&resp401(Some(r#"Bearer realm="HTTPS://evil.example/token""#)))
            .unwrap_err();
        assert!(e.to_string().contains("不在允许列表"), "{}", e);
    }

    // ---- parse_auth_params 健壮性 ----

    #[test]
    fn auth_params_handles_whitespace_and_unquoted() {
        let m = parse_auth_params(r#"Bearer realm = "https://a.example/t" , service=svc"#);
        assert_eq!(m.get("realm").unwrap(), "https://a.example/t");
        assert_eq!(m.get("service").unwrap(), "svc");
        // 无 Bearer 前缀（纯参数）也能解析
        let m = parse_auth_params(r#"realm="https://a.example/t""#);
        assert_eq!(m.get("realm").unwrap(), "https://a.example/t");
    }

    #[test]
    fn url_encode_passthrough_and_non_ascii() {
        // unreserved 字符原样
        assert_eq!(url_encode("abcXYZ019-_.~"), "abcXYZ019-_.~");
        // 非 ASCII 按字节百分号编码（'中' = E4 B8 AD）
        assert_eq!(url_encode("中"), "%E4%B8%AD");
        assert_eq!(url_encode("a b"), "a%20b");
    }

    #[test]
    fn url_host_extraction() {
        assert_eq!(url_host("https://a.b:443/x/y"), Some("a.b:443".to_string()));
        assert_eq!(url_host("https://A.B/"), Some("a.b".to_string())); // 小写化
        assert_eq!(url_host("no-scheme/x"), None);
        assert_eq!(url_host("https://"), None);
    }
}
