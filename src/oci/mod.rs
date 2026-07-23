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
}

/// 本地缓存根目录：Windows 用 %USERPROFILE%，其余用 $HOME。
pub fn cache_root() -> crate::error::Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
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
}
