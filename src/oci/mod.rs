//! OCI 镜像拉取与本地缓存。
//!
//! 实现 OCI Distribution Spec v2 的最小客户端（匿名拉取公开镜像）：
//! - `mod.rs`     —— 镜像引用解析（docker.io 补全规则）、缓存目录布局、`pull`/`list` 编排
//! - `registry.rs`—— HTTP/Bearer token 认证、manifest / blob 拉取
//! - `image.rs`   —— manifest list 选择、digest 校验、layer 解包与 whiteout 处理
//!
//! 缓存目录布局（跨平台抽象，Windows 下 `%USERPROFILE%\.wbox\...`）：
//! ```text
//! ~/.wbox/images/<registry>/<name>/<tag>/
//! ├── manifest.json   # 最终选中的 image manifest（非 index）
//! ├── config.json     # image config
//! ├── layers.json     # 层 digest 列表（元数据，供 list 显示）
//! └── rootfs/         # 按序解包 + whiteout 处理后的根文件系统
//! ```
//! 其中 `<registry>` 为 registry 主机、`<name>` 为仓库名把 `/` 替换为 `_`
//! （如 `library_ubuntu`）；路径段中的 `:`（registry 端口、digest 引用
//! `sha256:...`）一律替换为 `_`，避免 Windows 非法目录名（M4）。

pub mod config;
pub mod image;
pub mod registry;

use crate::error::{ErrKind, KindExt, WboxError};
use anyhow::Context;
use std::path::PathBuf;

/// Docker Hub 默认 registry 主机。
pub const DEFAULT_REGISTRY: &str = "registry-1.docker.io";

/// 解析后的镜像引用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    /// registry 主机（如 registry-1.docker.io、quay.io）
    pub registry: String,
    /// 仓库名（如 library/ubuntu）
    pub repo: String,
    /// tag 或 digest（digest 形式为 sha256:...）
    pub reference: String,
}

impl ImageRef {
    /// 解析镜像引用字符串，应用 docker 风格的补全规则：
    /// - 无 registry 前缀（首段不含 `.`/`:` 且非 localhost）→ 默认 docker.io；
    /// - docker.io 且仓库名无 `/` → 补 `library/`；
    /// - 无 tag/digest → `latest`。
    ///
    /// `registry_override`：CLI `--registry` 显式指定的主机，优先于引用内前缀。
    pub fn parse(s: &str, registry_override: Option<&str>) -> crate::error::Result<Self> {
        let s = s.trim();
        if s.is_empty() {
            return Err(WboxError::args("镜像引用不能为空"));
        }

        // 先拆出 @digest 或 :tag（注意 registry 端口里的冒号不算 tag 分隔符：
        // tag 冒号必须在最后一个 `/` 之后）。
        let (name_part, reference) = if let Some((n, d)) = s.rsplit_once('@') {
            (n.to_string(), d.to_string())
        } else {
            match s.rfind(':') {
                Some(i) if s.rfind('/').map_or(true, |j| i > j) => {
                    (s[..i].to_string(), s[i + 1..].to_string())
                }
                _ => (s.to_string(), "latest".to_string()),
            }
        };
        if name_part.is_empty() || reference.is_empty() {
            return Err(WboxError::args(format!("镜像引用 '{}' 格式不合法", s)));
        }

        // 拆 registry 前缀：首段含 '.' 或 ':' 或等于 localhost 时视为 registry 主机。
        let segs: Vec<&str> = name_part.splitn(2, '/').collect();
        let (registry, repo) = match (segs.len(), segs[0]) {
            (2, first)
                if first.contains('.') || first.contains(':') || first == "localhost" =>
            {
                (first.to_string(), segs[1].to_string())
            }
            _ => (DEFAULT_REGISTRY.to_string(), name_part.clone()),
        };
        if let Some(r) = registry_override {
            // override 时整个 name_part 视为仓库名；mirror 通常代理 docker hub，
            // 无命名空间时同样补 library/。
            let repo = if !name_part.contains('/') {
                format!("library/{}", name_part)
            } else {
                name_part.clone()
            };
            return Ok(Self {
                registry: r.to_string(),
                repo,
                reference,
            });
        }

        // docker.io 补全：无命名空间的官方镜像加 library/。
        let repo = if registry == DEFAULT_REGISTRY && !repo.contains('/') {
            format!("library/{}", repo)
        } else {
            repo
        };

        if repo.is_empty() {
            return Err(WboxError::args(format!("镜像引用 '{}' 缺少仓库名", s)));
        }
        Ok(Self {
            registry,
            repo,
            reference,
        })
    }

    /// 缓存目录名中的 name 部分（`/` → `_`）。
    pub fn cache_name(&self) -> String {
        self.repo.replace('/', "_")
    }

    /// 供展示的 `repo:tag`（digest 引用为 `repo@digest`）形式。
    pub fn repo_tag(&self) -> String {
        if self.reference.starts_with("sha256:") {
            format!("{}@{}", self.repo, self.reference)
        } else {
            format!("{}:{}", self.repo, self.reference)
        }
    }
}

/// 本地缓存根目录：Windows 用 %USERPROFILE%，其余用 $HOME。
pub fn cache_root() -> crate::error::Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("HOME").filter(|v| !v.is_empty()))
        .ok_or_else(|| WboxError::registry("无法确定用户主目录（USERPROFILE/HOME 均未设置）"))?;
    Ok(PathBuf::from(home).join(".wbox").join("images"))
}

/// 缓存目录路径段的净化：`:` 在 Windows 上是非法文件名字符
/// （registry 可能带端口、digest 引用形如 `sha256:...`），统一替换为 `_`（M4）。
fn sanitize_segment(s: &str) -> String {
    s.replace(':', "_")
}

/// 某个镜像引用的缓存目录。缓存键包含 registry（M5），
/// 避免不同 registry/mirror 的同名镜像互相覆盖。
pub fn image_dir(iref: &ImageRef) -> crate::error::Result<PathBuf> {
    Ok(cache_root()?
        .join(sanitize_segment(&iref.registry))
        .join(iref.cache_name())
        .join(sanitize_segment(&iref.reference)))
}

/// "镜像已 pull"的唯一判定入口：缓存目录存在且 rootfs 已解包。
/// `run` 目标判别（classify_target 的 is_pulled 回调）与 image rm
/// 等路径一律经此判定，避免各处重复拼接 `rootfs.is_dir()`。
pub fn is_pulled(iref: &ImageRef) -> bool {
    image_dir(iref)
        .map(|d| d.join("rootfs").is_dir())
        .unwrap_or(false)
}

/// `wbox image pull` 的入口编排。
pub fn pull(
    image_ref: &str,
    os: &str,
    arch: &str,
    registry_override: Option<&str>,
    verbose: bool,
) -> crate::error::Result<()> {
    let iref = ImageRef::parse(image_ref, registry_override)?;
    let dest = image_dir(&iref)?;

    println!(
        "wbox: 拉取 {}（registry={}, 平台 {}/{}）",
        iref.repo, iref.registry, os, arch
    );

    let client = registry::RegistryClient::new(&iref.registry);
    let summary = image::pull_image(&client, &iref, os, arch, &dest, verbose)?;

    // 双层隔离衔接：rootfs 默认 ACL 不含 AppContainer SID，
    // 需授予 ALL APPLICATION PACKAGES 读取权，容器内才能读到 rootfs（S2）。
    // 授权失败不否决已成功的 pull，但明确告警。
    #[cfg(windows)]
    if let Err(e) = crate::acl::grant_read_recursive(&dest.join("rootfs")) {
        eprintln!(
            "wbox: 警告: rootfs ACL 授权失败（{}）。\
             容器内可能读不到 rootfs；可手工执行：\
             icacls \"{}\" /grant \"*S-1-15-2-1:(OI)(CI)(RX)\" /T",
            e,
            dest.join("rootfs").display()
        );
    }

    println!("wbox: 完成 —— {} 层已解包到 {}", summary.layers, dest.join("rootfs").display());
    println!("wbox: manifest digest: {}", summary.manifest_digest);
    Ok(())
}

/// `wbox image list`：扫描缓存目录，列出已拉取的镜像。
pub fn list() -> crate::error::Result<u32> {
    let root = cache_root()?;
    if !root.is_dir() {
        println!("（缓存为空：{} 不存在）", root.display());
        return Ok(0);
    }
    println!("{:<28} {:<32} {:<20} LAYERS", "REGISTRY", "IMAGE", "TAG");
    let mut count = 0u32;
    let mut registries: Vec<_> = std::fs::read_dir(&root)
        .context("读取镜像缓存目录失败")
        .ctx(ErrKind::Registry)?
        .filter_map(|e| e.ok())
        .collect();
    registries.sort_by_key(|e| e.file_name());
    for reg_entry in registries {
        let registry = reg_entry.file_name().to_string_lossy().into_owned();
        let mut names: Vec<_> = std::fs::read_dir(reg_entry.path())
            .map(|rd| rd.filter_map(|e| e.ok()).collect())
            .unwrap_or_default();
        names.sort_by_key(|e| e.file_name());
        for name_entry in names {
            let name = name_entry.file_name().to_string_lossy().into_owned();
            let mut tags: Vec<_> = std::fs::read_dir(name_entry.path())
                .map(|rd| rd.filter_map(|e| e.ok()).collect())
                .unwrap_or_default();
            tags.sort_by_key(|e| e.file_name());
            for tag_entry in tags {
                let tag = tag_entry.file_name().to_string_lossy().into_owned();
                // 读 layers.json 拿层数；失败则显示 "-"
                let layers = std::fs::read_to_string(tag_entry.path().join("layers.json"))
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .and_then(|v| v.as_array().map(|a| a.len().to_string()))
                    .unwrap_or_else(|| "-".to_string());
                println!("{:<28} {:<32} {:<20} {}", registry, name, tag, layers);
                count += 1;
            }
        }
    }
    if count == 0 {
        println!("（无已缓存镜像）");
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dockerhub_short() {
        let r = ImageRef::parse("ubuntu:24.04", None).unwrap();
        assert_eq!(r.registry, DEFAULT_REGISTRY);
        assert_eq!(r.repo, "library/ubuntu");
        assert_eq!(r.reference, "24.04");
    }

    #[test]
    fn parse_default_tag() {
        let r = ImageRef::parse("hello-world", None).unwrap();
        assert_eq!(r.repo, "library/hello-world");
        assert_eq!(r.reference, "latest");
    }

    #[test]
    fn parse_namespaced_dockerhub() {
        let r = ImageRef::parse("prom/busybox:latest", None).unwrap();
        assert_eq!(r.registry, DEFAULT_REGISTRY);
        assert_eq!(r.repo, "prom/busybox");
    }

    #[test]
    fn parse_explicit_registry() {
        let r = ImageRef::parse("quay.io/prometheus/busybox:latest", None).unwrap();
        assert_eq!(r.registry, "quay.io");
        assert_eq!(r.repo, "prometheus/busybox");
        assert_eq!(r.reference, "latest");
    }

    #[test]
    fn parse_registry_with_port_and_digest() {
        let r = ImageRef::parse("localhost:5000/ns/app@sha256:abc", None).unwrap();
        assert_eq!(r.registry, "localhost:5000");
        assert_eq!(r.repo, "ns/app");
        assert_eq!(r.reference, "sha256:abc");
    }

    #[test]
    fn parse_registry_override() {
        let r = ImageRef::parse("ubuntu:24.04", Some("docker.m.daocloud.io")).unwrap();
        assert_eq!(r.registry, "docker.m.daocloud.io");
        assert_eq!(r.repo, "library/ubuntu"); // mirror 代理 docker hub，同样补 library/
        assert_eq!(r.reference, "24.04");
    }

    // ---- M4/M5：缓存键含 registry，路径段不含 Windows 非法字符 ':' ----

    #[test]
    fn image_dir_includes_registry_and_sanitizes_colon() {
        let r = ImageRef::parse("localhost:5000/ns/app@sha256:abc", None).unwrap();
        let d = image_dir(&r).unwrap();
        let s = d.to_string_lossy();
        // digest 引用与 registry 端口中的 ':' 必须替换为 '_'（Windows 合法目录名）
        assert!(s.contains("localhost_5000"), "{}", s);
        assert!(s.ends_with("sha256_abc"), "{}", s);
        // 缓存键含 registry：不同 registry 同 repo/tag 目录不同
        let a = image_dir(&ImageRef::parse("quay.io/ns/app:1", None).unwrap()).unwrap();
        let b = image_dir(&ImageRef::parse("docker.io/ns/app:1", None).unwrap()).unwrap();
        assert_ne!(a, b);
    }

    // ---- ImageRef 解析边界 ----

    #[test]
    fn parse_digest_reference_dockerhub() {
        // digest 引用走 @ 分隔，docker hub 补全照常
        let r = ImageRef::parse("ubuntu@sha256:0123456789abcdef", None).unwrap();
        assert_eq!(r.registry, DEFAULT_REGISTRY);
        assert_eq!(r.repo, "library/ubuntu");
        assert_eq!(r.reference, "sha256:0123456789abcdef");
    }

    #[test]
    fn parse_registry_with_port_and_tag() {
        // registry 端口里的冒号不是 tag 分隔符（tag 冒号在最后一个 / 之后）
        let r = ImageRef::parse("localhost:5000/app:1.2.3", None).unwrap();
        assert_eq!(r.registry, "localhost:5000");
        assert_eq!(r.repo, "app");
        assert_eq!(r.reference, "1.2.3");
    }

    #[test]
    fn parse_multi_level_repo_path() {
        let r = ImageRef::parse("quay.io/org/team/app:dev", None).unwrap();
        assert_eq!(r.registry, "quay.io");
        assert_eq!(r.repo, "org/team/app");
        assert_eq!(r.reference, "dev");
        // 多级路径 docker hub 也保持原样（已含 / 不补 library）
        let r = ImageRef::parse("a/b/c:1", None).unwrap();
        assert_eq!(r.registry, DEFAULT_REGISTRY);
        assert_eq!(r.repo, "a/b/c");
    }

    #[test]
    fn parse_uppercase_and_localhost_variants() {
        // 大写字母原样保留（当前不做规范化，与 docker 不同——记录现状）
        let r = ImageRef::parse("Library/Ubuntu:LATEST", None).unwrap();
        assert_eq!(r.repo, "Library/Ubuntu");
        assert_eq!(r.reference, "LATEST");
        // localhost 精确匹配视为 registry；LOCALHOST 大小写不命中（记录现状：
        // 首段无 . / : 时回退 docker hub 仓库名）
        let r = ImageRef::parse("localhost/app:1", None).unwrap();
        assert_eq!(r.registry, "localhost");
        let r = ImageRef::parse("LOCALHOST/app:1", None).unwrap();
        assert_eq!(r.registry, DEFAULT_REGISTRY);
        assert_eq!(r.repo, "LOCALHOST/app");
    }

    #[test]
    fn parse_rejects_malformed_refs() {
        // 空引用 / 空白引用
        assert!(ImageRef::parse("", None).is_err());
        assert!(ImageRef::parse("   ", None).is_err());
        // tag 为空（ubuntu:）
        assert!(ImageRef::parse("ubuntu:", None).is_err());
        // digest 为空（ubuntu@）
        assert!(ImageRef::parse("ubuntu@", None).is_err());
        // 名为空（@sha256:x）
        assert!(ImageRef::parse("@sha256:x", None).is_err());
        // registry 后无仓库名
        assert!(ImageRef::parse("quay.io/", None).is_err());
        assert!(ImageRef::parse("quay.io/:tag", None).is_err());
    }

    #[test]
    fn parse_trimmed_whitespace() {
        let r = ImageRef::parse("  ubuntu:24.04  ", None).unwrap();
        assert_eq!(r.repo, "library/ubuntu");
        assert_eq!(r.reference, "24.04");
    }

    #[test]
    fn parse_registry_override_keeps_namespace() {
        // override 时整段视为仓库名：有命名空间保持，无命名空间补 library/
        let r = ImageRef::parse("ns/app:2", Some("mirror.local")).unwrap();
        assert_eq!(r.registry, "mirror.local");
        assert_eq!(r.repo, "ns/app");
        let r = ImageRef::parse("quay.io/ns/app:2", Some("mirror.local")).unwrap();
        // override 优先于引用内 registry 前缀，整段（含原 registry 字面量）作仓库名
        assert_eq!(r.registry, "mirror.local");
        assert_eq!(r.repo, "quay.io/ns/app");
    }

    #[test]
    fn repo_tag_formats_tag_and_digest() {
        let r = ImageRef::parse("ubuntu:24.04", None).unwrap();
        assert_eq!(r.repo_tag(), "library/ubuntu:24.04");
        let r = ImageRef::parse("ubuntu@sha256:abc", None).unwrap();
        assert_eq!(r.repo_tag(), "library/ubuntu@sha256:abc");
    }

    #[test]
    fn cache_name_flattens_slashes() {
        let r = ImageRef::parse("quay.io/org/team/app:1", None).unwrap();
        assert_eq!(r.cache_name(), "org_team_app");
    }

    #[test]
    fn image_dir_layout_segments() {
        // 布局 <root>/<registry>/<name_flat>/<reference>，全段无 ':'
        let r = ImageRef::parse("localhost:5000/ns/app@sha256:deadbeef", None).unwrap();
        let d = image_dir(&r).unwrap();
        let segs: Vec<String> = d
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        for s in &segs {
            assert!(!s.contains(':'), "路径段含 Windows 非法字符 ':'：{}", s);
        }
        let tail: Vec<&str> = segs.iter().rev().take(3).map(|s| s.as_str()).collect();
        assert_eq!(tail, vec!["sha256_deadbeef", "ns_app", "localhost_5000"]);
    }
}
