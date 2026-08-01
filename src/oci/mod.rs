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

pub mod archive;
pub mod config;
pub mod image;
pub mod push;
pub mod registry;

use crate::error::{ErrKind, KindExt, WboxError};
use crate::fault::Context;
use std::path::PathBuf;

/// 默认拉取的 OCI 架构。
///
/// 规则**按宿主与执行方式分开**，不是简单地"跟着编译目标走"：
///
/// - **Windows 宿主**恒为 `amd64`。那里 Linux 镜像跑在 `wbox-linux` 上，
///   而它是个 **x86-64** 模拟器——拉 arm64 镜像进去一条指令都执行不了。
///   即便将来 wbox 自身编成 arm64 Windows，这一条也不变。
/// - **Linux 宿主**跟随本机架构。那里走的是原生 namespace 容器，镜像里的
///   二进制由**真 CPU** 执行，架构不符就是 `Exec format error`。
/// - **macOS 宿主**近期路线仍是 x86-64 `wbox-linux`，因此默认 `amd64`；不能因
///   Apple Silicon 宿主是 AArch64 就谎称 AArch64 guest runtime 已经可用。
///
/// 之前两处都硬编码 `amd64`，在 arm64 Linux 上会拉下一个根本跑不起来的
/// 镜像，而且报错发生在容器内部，很难联想到是架构选错了。
pub fn default_arch() -> &'static str {
    default_arch_for(crate::platform::current_host(), std::env::consts::ARCH)
}

fn default_arch_for(host: Option<crate::platform::HostOs>, target_arch: &str) -> &'static str {
    if host != Some(crate::platform::HostOs::Linux) {
        return "amd64";
    }
    match target_arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "arm" => "arm",
        "powerpc64" => "ppc64le",
        "s390x" => "s390x",
        "riscv64" => "riscv64",
        // 认不出来就退回 amd64：OCI 的架构名与 Rust 的不是一一对应，
        // 猜一个错的不如退回最普遍的那个，让用户用 --arch 显式指定。
        _ => "amd64",
    }
}

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
                Some(i) if s.rfind('/').is_none_or(|j| i > j) => {
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
            (2, first) if first.contains('.') || first.contains(':') || first == "localhost" => {
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
        // 先扁平化 `/`（多级仓库名 org/team/app → org_team_app），再过一遍
        // 净化：repo 也可能含 `\` 或纯点段，同样不得改变路径层级。
        sanitize_segment(&self.repo.replace('/', "_"))
    }

    /// 供展示的 `repo:tag`（digest 引用为 `repo@digest`）形式。
    pub fn repo_tag(&self) -> String {
        if self.reference.starts_with("sha256:") {
            format!("{}@{}", self.repo, self.reference)
        } else {
            format!("{}:{}", self.repo, self.reference)
        }
    }

    /// 可持久化的规范引用：Docker Hub 保持现有短形式，其他 registry 不得丢失。
    pub fn qualified_ref(&self) -> String {
        if self.registry == DEFAULT_REGISTRY {
            self.repo_tag()
        } else if self.reference.starts_with("sha256:") {
            format!("{}/{}@{}", self.registry, self.repo, self.reference)
        } else {
            format!("{}/{}:{}", self.registry, self.repo, self.reference)
        }
    }
}

/// 本地缓存根目录：Windows 用 %USERPROFILE%，其余用 $HOME。
pub fn cache_root() -> crate::error::Result<PathBuf> {
    let root = crate::paths::root()
        .map_err(|error| WboxError::registry(format!("无法确定用户主目录：{error}")))?;
    Ok(root.join("images"))
}

/// 缓存目录路径段的净化。
///
/// 这是 [`image_dir`] 的**安全边界**：`image pull` 往该目录解包、
/// `image rm` 对它**递归删除**，所以任何一段都不得改变路径层级。
/// 原实现只把 `:` 换成 `_`（Windows 非法文件名字符：registry 带端口、
/// digest 形如 `sha256:...`），对 `..` 一概放行——而 `..` 含点，会被
/// registry 判定当作主机名，于是 `wbox image rm ../evil` 的目标变成
/// `<缓存根>/../evil`，逃出缓存根（单测 image_dir_never_escapes_cache_root
/// 实测捕获）。
///
/// 故此处额外消灭两类形态：
/// - 路径分隔符 `/` 与 `\`（`\` 在 Windows 上同样是分隔符）；
/// - 纯点段 `.` / `..` / `...`（前缀 `_` 降级为普通名字；全点名在
///   Windows 上本就有额外限制）。
fn sanitize_segment(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| match c {
            ':' | '/' | '\\' => '_',
            c => c,
        })
        .collect();
    if out.is_empty() || out.chars().all(|c| c == '.') {
        format!("_{}", out)
    } else {
        out
    }
}

/// 某个镜像引用的缓存目录。缓存键包含 registry（M5），
/// 避免不同 registry/mirror 的同名镜像互相覆盖。
pub fn image_dir(iref: &ImageRef) -> crate::error::Result<PathBuf> {
    Ok(cache_root()?
        .join(sanitize_segment(&iref.registry))
        .join(iref.cache_name())
        .join(sanitize_segment(&iref.reference)))
}

/// 原始压缩层 blob 的存放目录（相对镜像缓存目录）。
///
/// pull 时**除了解包，还原样留一份压缩层**。多花一份磁盘换来两件解包结果给不了
/// 的事：`push` 能原样回推（manifest digest 不变，与上游共享层），以及将来
/// `FROM` 复用基础层。没有它，push 只能把 rootfs 压平成单层（F9.13）。
pub const BLOBS_DIR: &str = "blobs";

/// digest → blob 文件名。`sha256:abc` → `sha256_abc`：冒号在 Windows
/// 路径里非法，而缓存布局要跨平台一致。
pub fn blob_file_name(digest: &str) -> String {
    digest.replace(':', "_")
}

/// blob 在某个镜像缓存目录下的路径。
pub fn blob_path(image_dir: &std::path::Path, digest: &str) -> PathBuf {
    image_dir.join(BLOBS_DIR).join(blob_file_name(digest))
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

    println!(
        "wbox: 完成 —— {} 层已解包到 {}",
        summary.layers,
        dest.join("rootfs").display()
    );
    println!("wbox: manifest digest: {}", summary.manifest_digest);
    Ok(())
}

/// 缓存里的一个镜像。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedImage {
    /// **可直接喂回给别的命令**的引用（`wbox rmi`/`run`/`push` 都收）。
    pub reference: String,
    pub registry: String,
    pub tag: String,
    /// 层数；读不到时为 "-"。
    pub layers: String,
    /// Exact cache directory; consumers must not reconstruct it from display text.
    pub directory: PathBuf,
}

/// 从缓存目录三元组还原一个**可用的**镜像引用。
///
/// # 为什么要"还原"而不是直接显示目录名
///
/// 缓存目录名是 [`ImageRef::cache_name`] 产出的：`library/demo` → `library_demo`。
/// 此前 `wbox images` 直接把它印在 IMAGE 列里，于是用户看到 `library_demo`、
/// 照抄去 `wbox rmi library_demo`，得到的是「镜像 'library/library_demo' 未 pull」
/// ——**列出来的名字喂不回给任何命令**。
///
/// # 还原是有歧义的，但这里的做法是可证的
///
/// `_` 既可能来自 `/` 的扁平化，也可能本来就在仓库名里，单看目录名分不清。
/// 但这不影响正确性：还原出候选引用后**再解析回去、算一遍缓存目录**，
/// 只有算出来与实际目录一致才采用。既然 `rmi`/`run` 也都经同一个
/// [`image_dir`] 去定位，能这样往返的引用就一定指向这个目录——歧义留下的
/// 多个候选，操作上是等价的。
///
/// 往返对不上（缓存被手工改过、跨版本布局变化等）时返回 `None`，
/// 调用方退回显示原始目录名并如实标注，而不是给一个用不了的引用。
fn restore_reference(registry: &str, dir_name: &str, tag: &str) -> Option<String> {
    let repo = dir_name.replace('_', "/");
    let candidate = if registry == DEFAULT_REGISTRY {
        format!("{}:{}", repo, tag)
    } else {
        format!("{}/{}:{}", registry, repo, tag)
    };
    let parsed = ImageRef::parse(&candidate, None).ok()?;
    let ok = sanitize_segment(&parsed.registry) == registry
        && parsed.cache_name() == dir_name
        && sanitize_segment(&parsed.reference) == tag;
    ok.then_some(candidate)
}

/// 枚举缓存里的镜像。
///
/// 与打印分开：`wbox images` 要表格、`wbox images -q` 要裸引用、以后若要
/// 别的用途也不必再抄一遍目录遍历。此前这段逻辑焊死在打印函数里，
/// 想复用只能复制。
pub fn list_refs() -> crate::error::Result<Vec<CachedImage>> {
    let root = cache_root()?;
    let mut out = Vec::new();
    if !root.is_dir() {
        return Ok(out);
    }
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
            let dir_name = name_entry.file_name().to_string_lossy().into_owned();
            let mut tags: Vec<_> = std::fs::read_dir(name_entry.path())
                .map(|rd| rd.filter_map(|e| e.ok()).collect())
                .unwrap_or_default();
            tags.sort_by_key(|e| e.file_name());
            for tag_entry in tags {
                let tag = tag_entry.file_name().to_string_lossy().into_owned();
                // 读 layers.json 拿层数；失败则显示 "-"
                let layers = std::fs::read_to_string(tag_entry.path().join("layers.json"))
                    .ok()
                    .and_then(|s| wbox_codec::json::from_str(&s).ok())
                    .and_then(|v| v.as_array().map(|a| a.len().to_string()))
                    .unwrap_or_else(|| "-".to_string());
                let reference = restore_reference(&registry, &dir_name, &tag)
                    // 还原不出来时如实标注，而不是给一个用不了的引用
                    .unwrap_or_else(|| format!("{}（缓存目录名，引用无法还原）", dir_name));
                out.push(CachedImage {
                    reference,
                    registry: registry.clone(),
                    tag,
                    layers,
                    directory: tag_entry.path(),
                });
            }
        }
    }
    Ok(out)
}

/// `wbox image list`：列出已拉取的镜像。
pub fn list() -> crate::error::Result<u32> {
    let root = cache_root()?;
    if !root.is_dir() {
        println!("（缓存为空：{} 不存在）", root.display());
        return Ok(0);
    }
    let rows = list_refs()?;
    // IMAGE 列给的是**能直接照抄去用**的引用，不是缓存目录名
    println!("{:<28} {:<40} {:<20} LAYERS", "REGISTRY", "IMAGE", "TAG");
    for r in &rows {
        println!(
            "{:<28} {:<40} {:<20} {}",
            r.registry, r.reference, r.tag, r.layers
        );
    }
    if rows.is_empty() {
        println!("（无已缓存镜像）");
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    /// 架构默认值必须跟随 execution provider，而不是笼统跟随宿主 CPU。
    #[test]
    fn default_arch_follows_host_execution_route() {
        use crate::platform::HostOs::{Linux, Macos, Windows};

        assert_eq!(default_arch_for(Some(Windows), "aarch64"), "amd64");
        assert_eq!(default_arch_for(Some(Macos), "aarch64"), "amd64");
        assert_eq!(default_arch_for(Some(Linux), "x86_64"), "amd64");
        assert_eq!(default_arch_for(Some(Linux), "aarch64"), "arm64");
        assert_eq!(default_arch_for(Some(Linux), "riscv64"), "riscv64");
        assert_eq!(default_arch_for(None, "aarch64"), "amd64");
        assert!(!default_arch().is_empty());
    }

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

    /// `image rm` 会对 `image_dir()` 的返回值做**递归删除**，因此"解析后的
    /// 引用不可能指到缓存根之外"是一条安全边界，必须有测试守住。
    ///
    /// 断言两条：路径里不得出现 `..` 组件（仅靠 `starts_with` 会被
    /// `<root>/a/../../evil` 骗过——它按组件比较，前缀确实匹配），
    /// 且必须落在 `<root>/.wbox/images` 之内。
    #[test]
    fn image_dir_never_escapes_cache_root() {
        use std::path::Component;
        let home = crate::testenv::TempHome::new("oci-escape");
        let root = home.dir.join(".wbox").join("images");
        for hostile in [
            "../evil",
            "../../etc",
            "a/../../evil",
            "..",
            "ubuntu:../../evil",
            "reg.io/../../evil",
            "reg.io/ns/../../../evil",
            "./evil",
            "evil/./x",
            r"..\evil",
        ] {
            let Ok(r) = ImageRef::parse(hostile, None) else {
                continue; // 被解析拒绝同样满足安全要求
            };
            let dir = image_dir(&r).unwrap();
            assert!(
                !dir.components().any(|c| c == Component::ParentDir),
                "引用 {:?} 的缓存目录含 `..` 组件：{}",
                hostile,
                dir.display()
            );
            assert!(
                dir.starts_with(&root),
                "引用 {:?} 的缓存目录逃出缓存根：{}（根 {}）",
                hostile,
                dir.display(),
                root.display()
            );
        }
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
        assert_eq!(r.qualified_ref(), "library/ubuntu:24.04");
        let r = ImageRef::parse("ubuntu@sha256:abc", None).unwrap();
        assert_eq!(r.repo_tag(), "library/ubuntu@sha256:abc");
        let r = ImageRef::parse("quay.io/acme/app:2", None).unwrap();
        assert_eq!(r.qualified_ref(), "quay.io/acme/app:2");
        let r = ImageRef::parse("localhost:5000/app@sha256:abc", None).unwrap();
        assert_eq!(r.qualified_ref(), "localhost:5000/app@sha256:abc");
    }

    /// 列出来的引用必须**喂得回去**：还原 → 解析 → 再算缓存目录，必须回到原处。
    /// 此前 IMAGE 列印的是缓存目录名（`library_demo`），照抄去 rmi 会得到
    /// 「镜像 'library/library_demo' 未 pull」——列出来的名字谁都用不了。
    #[test]
    fn restored_reference_round_trips_to_the_same_cache_dir() {
        let r = restore_reference(DEFAULT_REGISTRY, "library_demo", "latest").unwrap();
        assert_eq!(r, "library/demo:latest");
        let back = ImageRef::parse(&r, None).unwrap();
        assert_eq!(back.cache_name(), "library_demo");

        // 多级仓库名同样要往返得回来
        let r = restore_reference("quay.io", "org_team_app", "1").unwrap();
        assert_eq!(r, "quay.io/org/team/app:1");
        assert_eq!(
            ImageRef::parse(&r, None).unwrap().cache_name(),
            "org_team_app"
        );

        // 往返对不上的（这里用一个空目录名）不许硬给引用
        assert!(restore_reference(DEFAULT_REGISTRY, "", "latest").is_none());
    }

    #[test]
    fn cache_name_flattens_slashes() {
        let r = ImageRef::parse("quay.io/org/team/app:1", None).unwrap();
        assert_eq!(r.cache_name(), "org_team_app");
    }

    #[test]
    fn image_dir_layout_segments() {
        // image_dir 自己拼的 3 段（<registry>/<name_flat>/<reference>）必须无 ':'',
        // 这是 sanitize_segment 的契约。不遍历 cache_root() 整条绝对路径：
        // Windows 上它带盘符前缀（如 "C:"），那是系统 home 的属性，与本函数无关。
        let r = ImageRef::parse("localhost:5000/ns/app@sha256:deadbeef", None).unwrap();
        let d = image_dir(&r).unwrap();
        let root = cache_root().unwrap();
        let segs: Vec<String> = d
            .strip_prefix(&root)
            .expect("image_dir 必在 cache_root 之下")
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        for s in &segs {
            assert!(!s.contains(':'), "路径段含 Windows 非法字符 ':'：{}", s);
        }
        assert_eq!(
            segs,
            vec!["localhost_5000", "ns_app", "sha256_deadbeef"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }
}
