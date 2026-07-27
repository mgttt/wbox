//! 镜像层处理：manifest list 选择、sha256 digest 校验、tar(+gzip) 解包、whiteout。
//!
//! Whiteout 规则（OCI / overlayfs 约定）：
//! - 文件 `.wh.<name>`：删除下层已有的 `<name>`（文件或目录），自身不落盘；
//! - 文件 `.wh..wh..opq`：opaque 目录，清空该目录下所有下层内容，自身不落盘。

use super::registry::RegistryClient;
use super::ImageRef;
use crate::error::{ErrKind, KindExt, WboxError};
use anyhow::Context;
use sha2::Digest;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// manifest list / index 的 media type 判断。
fn is_index(media_type: &str) -> bool {
    media_type == "application/vnd.oci.image.index.v1+json"
        || media_type == "application/vnd.docker.distribution.manifest.list.v2+json"
}

/// 拉取结果摘要（供 CLI 打印与测试断言）。
pub struct PullSummary {
    /// 实际解包的层数
    pub layers: usize,
    /// 最终 image manifest 的 digest
    pub manifest_digest: String,
}

static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

fn sibling_path(dest: &Path, role: &str, unique: bool) -> anyhow::Result<PathBuf> {
    let parent = dest
        .parent()
        .ok_or_else(|| anyhow::anyhow!("镜像缓存路径缺少父目录：{}", dest.display()))?;
    let name = dest
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("镜像缓存路径缺少目录名：{}", dest.display()))?
        .to_string_lossy();
    if unique {
        let serial = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        Ok(parent.join(format!(
            ".{}.wbox-{}-{}-{}",
            name,
            role,
            std::process::id(),
            serial
        )))
    } else {
        Ok(parent.join(format!(".{}.wbox-{}", name, role)))
    }
}

struct StagingDir {
    path: PathBuf,
    armed: bool,
}

impl StagingDir {
    fn create(dest: &Path) -> anyhow::Result<Self> {
        let parent = dest
            .parent()
            .ok_or_else(|| anyhow::anyhow!("镜像缓存路径缺少父目录：{}", dest.display()))?;
        std::fs::create_dir_all(parent)?;
        let path = sibling_path(dest, "staging", true)?;
        std::fs::create_dir(&path)?;
        Ok(Self { path, armed: true })
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if self.armed {
            std::fs::remove_dir_all(&self.path).ok();
        }
    }
}

struct PullCommitLock {
    path: PathBuf,
}

impl PullCommitLock {
    fn acquire(dest: &Path, timeout: Duration) -> anyhow::Result<Self> {
        let path = sibling_path(dest, "pull.lock", false)?;
        let deadline = Instant::now() + timeout;
        loop {
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Instant::now() >= deadline {
                        anyhow::bail!(
                            "等待镜像缓存提交锁超时：{}（可能有另一 pull 正在提交）",
                            path.display()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl Drop for PullCommitLock {
    fn drop(&mut self) {
        std::fs::remove_dir(&self.path).ok();
    }
}

fn replace_cache_dir_with(
    staging: &Path,
    dest: &Path,
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> anyhow::Result<()> {
    if !dest.exists() {
        rename(staging, dest).with_context(|| {
            format!(
                "提交镜像缓存失败：{} -> {}",
                staging.display(),
                dest.display()
            )
        })?;
        return Ok(());
    }

    let backup = sibling_path(dest, "backup", true)?;
    rename(dest, &backup).with_context(|| {
        format!(
            "备份旧镜像缓存失败：{} -> {}",
            dest.display(),
            backup.display()
        )
    })?;

    if let Err(install_error) = rename(staging, dest) {
        if let Err(rollback_error) = rename(&backup, dest) {
            anyhow::bail!(
                "提交新镜像缓存失败：{}；回滚旧缓存也失败：{}。旧缓存保留在 {}",
                install_error,
                rollback_error,
                backup.display()
            );
        }
        return Err(anyhow::anyhow!(
            "提交新镜像缓存失败，旧缓存已恢复：{}",
            install_error
        ));
    }

    if let Err(e) = std::fs::remove_dir_all(&backup) {
        eprintln!(
            "wbox: 警告：新镜像已提交，但旧缓存备份清理失败 {}：{}",
            backup.display(),
            e
        );
    }
    Ok(())
}

fn replace_cache_dir(staging: &Path, dest: &Path) -> anyhow::Result<()> {
    replace_cache_dir_with(staging, dest, |from, to| std::fs::rename(from, to))
}

/// 拉取一个镜像并解包到 `dest`（dest 下生成 rootfs/ 与元数据文件）。
pub fn pull_image(
    client: &RegistryClient,
    iref: &ImageRef,
    os: &str,
    arch: &str,
    dest: &Path,
    verbose: bool,
) -> crate::error::Result<PullSummary> {
    let mut staging = StagingDir::create(dest)
        .context("创建镜像 staging 目录失败")
        .ctx(ErrKind::Registry)?;

    // ---- 1. 取 manifest（可能是 index）----
    let (ctype, body, header_digest) = client.get_manifest(&iref.repo, &iref.reference)?;
    // 按 digest 取回的内容（index 或 manifest）一律先过 digest 校验，防止 digest 链断裂。
    if iref.reference.starts_with("sha256:") {
        verify_digest(&iref.reference, &body)
            .context("manifest/index digest 校验失败")
            .ctx(ErrKind::Registry)?;
    }
    let (manifest_bytes, manifest_digest) = if is_index(&ctype) {
        // manifest list：按 os/arch 选择子 manifest（Windows 宿主默认拉 linux/amd64）
        let index: serde_json::Value = serde_json::from_slice(&body)
            .context("解析 manifest index 失败")
            .ctx(ErrKind::Registry)?;
        let digest = select_manifest(&index, os, arch)?;
        if verbose {
            println!("wbox: manifest list 选择 {}/{} -> {}", os, arch, digest);
        }
        let (ctype2, body2, hd2) = client.get_manifest(&iref.repo, &digest)?;
        if is_index(&ctype2) {
            return Err(WboxError::registry(
                "子 manifest 仍是 index（嵌套 index 不支持）",
            ));
        }
        // 子 manifest 是按 digest 取回的，必须校验该 digest（H2）。
        verify_digest(&digest, &body2)
            .context("子 manifest digest 校验失败")
            .ctx(ErrKind::Registry)?;
        let d = hd2.unwrap_or_else(|| sha256_hex_prefixed(&body2));
        (body2, d)
    } else {
        let d = header_digest.unwrap_or_else(|| sha256_hex_prefixed(&body));
        (body, d)
    };

    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)
        .context("解析 image manifest 失败")
        .ctx(ErrKind::Registry)?;

    // ---- 2. 拉取 config blob ----
    let config_digest = manifest
        .pointer("/config/digest")
        .and_then(|v| v.as_str())
        .ok_or_else(|| WboxError::registry("manifest 缺少 config.digest"))?;
    let config_bytes = client.get_blob(&iref.repo, config_digest)?;
    verify_digest(config_digest, &config_bytes)
        .context("config digest 校验失败")
        .ctx(ErrKind::Registry)?;
    if verbose {
        println!(
            "wbox: config {} 校验通过（{} 字节）",
            config_digest,
            config_bytes.len()
        );
    }

    // ---- 3. 准备 staging rootfs；成功提交前绝不修改现有缓存 ----
    let rootfs = staging.path.join("rootfs");
    std::fs::create_dir_all(&rootfs)
        .context("创建 staging rootfs 目录失败")
        .ctx(ErrKind::Registry)?;

    // ---- 4. 逐层拉取、校验、解包 ----
    let layers = manifest
        .get("layers")
        .and_then(|v| v.as_array())
        .ok_or_else(|| WboxError::registry("manifest 缺少 layers 数组"))?;
    let mut layer_digests = Vec::new();
    let mut symlinks = SymlinkState::default();
    for (i, layer) in layers.iter().enumerate() {
        let digest = layer
            .get("digest")
            .and_then(|v| v.as_str())
            .ok_or_else(|| WboxError::registry(format!("layer {} 缺少 digest", i)))?;
        println!(
            "wbox: [{}/{}] 拉取层 {} …",
            i + 1,
            layers.len(),
            &digest[..digest.len().min(19)]
        );
        let media_type = layer
            .get("mediaType")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let blob = client.get_blob(&iref.repo, digest)?;
        verify_digest(digest, &blob)
            .context(format!("layer {} digest 校验失败", digest))
            .ctx(ErrKind::Registry)?;
        unpack_layer_with_state(&blob, &rootfs, media_type, &mut symlinks)
            .context(format!("解包层 {} 失败", digest))
            .ctx(ErrKind::Registry)?;
        if verbose {
            println!("wbox:   校验通过，已解包（{} 字节）", blob.len());
        }
        layer_digests.push(digest.to_string());
    }
    finalize_symlinks(&rootfs, &symlinks)
        .context("实现镜像符号链接失败")
        .ctx(ErrKind::Registry)?;

    // ---- 5. 写元数据 ----
    std::fs::write(staging.path.join("manifest.json"), &manifest_bytes)
        .and_then(|_| std::fs::write(staging.path.join("config.json"), &config_bytes))
        .and_then(|_| {
            std::fs::write(
                staging.path.join("layers.json"),
                serde_json::to_string_pretty(&layer_digests).unwrap(),
            )
        })
        .context("写入镜像元数据失败")
        .ctx(ErrKind::Registry)?;

    // ---- 6. 同卷提交：同一目标串行交换，失败恢复旧缓存 ----
    let _commit_lock = PullCommitLock::acquire(dest, Duration::from_secs(300))
        .context("获取镜像缓存提交锁失败")
        .ctx(ErrKind::Registry)?;
    replace_cache_dir(&staging.path, dest)
        .context("替换镜像缓存失败")
        .ctx(ErrKind::Registry)?;
    staging.disarm();

    Ok(PullSummary {
        layers: layers.len(),
        manifest_digest,
    })
}

/// 从 manifest index 中挑选匹配 os/arch 的子 manifest digest。
fn select_manifest(
    index: &serde_json::Value,
    os: &str,
    arch: &str,
) -> crate::error::Result<String> {
    let manifests = index
        .get("manifests")
        .and_then(|v| v.as_array())
        .ok_or_else(|| WboxError::registry("index 缺少 manifests 数组"))?;
    for m in manifests {
        let platform = m.get("platform");
        let m_os = platform
            .and_then(|p| p.get("os"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let m_arch = platform
            .and_then(|p| p.get("architecture"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // 跳过 attestations 等 unknown 平台条目
        if m_os == os && m_arch == arch {
            return m
                .get("digest")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| WboxError::registry("匹配的子 manifest 缺少 digest"));
        }
    }
    let avail: Vec<String> = manifests
        .iter()
        .map(|m| {
            let p = m.get("platform");
            format!(
                "{}/{}",
                p.and_then(|p| p.get("os"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?"),
                p.and_then(|p| p.get("architecture"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
            )
        })
        .collect();
    Err(WboxError::registry(format!(
        "manifest list 中没有 {}/{} 平台（可用：{}）",
        os,
        arch,
        avail.join(", ")
    )))
}

/// sha256 digest 校验：`digest` 形如 `sha256:<hex>`。
pub fn verify_digest(digest: &str, data: &[u8]) -> anyhow::Result<()> {
    let expected = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow::anyhow!("不支持的 digest 算法（仅支持 sha256）：{}", digest))?;
    let actual = hex_sha256(data);
    anyhow::ensure!(
        actual == expected,
        "digest 不匹配：期望 sha256:{}，实际 sha256:{}",
        expected,
        actual
    );
    Ok(())
}

fn hex_sha256(data: &[u8]) -> String {
    let mut h = sha2::Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}

fn sha256_hex_prefixed(data: &[u8]) -> String {
    format!("sha256:{}", hex_sha256(data))
}

/// 判断 blob 是否 gzip（magic 1f 8b）。
fn is_gzip(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b
}

/// 判断 blob 是否 zstd（小端 magic 28 B5 2F FD）。
fn is_zstd(data: &[u8]) -> bool {
    data.len() >= 4 && data[..4] == [0x28, 0xb5, 0x2f, 0xfd]
}

/// 按 layer mediaType（辅以小端 magic 嗅探）把 blob 解成 tar 字节。
/// 显式拒绝不支持的压缩格式（zstd 等），而不是误当 tar 解出垃圾。
fn decompress_layer(blob: &[u8], media_type: &str) -> anyhow::Result<Vec<u8>> {
    let mt = media_type.to_ascii_lowercase();
    if mt.contains("zstd") || (mt.is_empty() && is_zstd(blob)) {
        anyhow::bail!(
            "不支持的压缩格式：zstd（mediaType={}），仅支持 gzip 与未压缩 tar",
            media_type
        );
    }
    let want_gzip = mt.contains("gzip") || (mt.is_empty() && is_gzip(blob));
    if want_gzip {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(blob).read_to_end(&mut out)?;
        Ok(out)
    } else if mt.is_empty() || mt.ends_with("+tar") || mt.contains("tar") || mt.contains("layer") {
        // 未压缩 tar：application/vnd.oci.image.layer.v1.tar、
        // application/vnd.docker.image.rootfs.diff.tar 等。
        Ok(blob.to_vec())
    } else {
        anyhow::bail!("不支持的 layer 压缩/格式：mediaType={}", media_type)
    }
}

/// 把 symlink 目标归一化为 rootfs 相对路径。
///
/// Linux 绝对目标（如 Alpine 的 `/bin/sh -> /bin/busybox`）以容器根为锚点，
/// 不能按 Windows 宿主绝对路径解释。相对目标则从链接所在目录开始解析。
/// Windows drive/UNC prefix 与越过 rootfs 的 `..` 均拒绝。
fn normalize_symlink_target(parent_rel: &Path, target: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut normalized = if target.has_root() {
        PathBuf::new()
    } else {
        parent_rel.to_path_buf()
    };
    for c in target.components() {
        match c {
            Component::Prefix(_) => return None,
            Component::RootDir => normalized.clear(),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Normal(n) => normalized.push(n),
        }
    }
    Some(normalized)
}

/// 校验 symlink 条目的目标按容器路径语义解析后不越出 rootfs。
fn symlink_target_in_scope(parent_rel: &Path, target: &Path) -> bool {
    normalize_symlink_target(parent_rel, target).is_some()
}

#[derive(Default)]
struct SymlinkState {
    /// 链接路径 -> OCI 条目中的原始目标。整个镜像的所有层共享此表。
    links: BTreeMap<PathBuf, PathBuf>,
}

impl SymlinkState {
    fn remove_at_or_below(&mut self, rel: &Path) {
        self.links
            .retain(|link, _| link != rel && !link.starts_with(rel));
    }

    fn remove_below(&mut self, rel: &Path) {
        self.links
            .retain(|link, _| link == rel || !link.starts_with(rel));
    }
}

/// 与 `resolve_in_scope` 相同，但在文件系统之外同时解析尚未落地的 OCI symlink。
/// 这使后续层通过目录链接写入时仍落到真实目标，而不是写进提前复制的快照。
fn resolve_with_symlinks(
    root: &Path,
    rel: &Path,
    symlinks: &SymlinkState,
) -> anyhow::Result<PathBuf> {
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::path::Component;

    let mut pending: VecDeque<OsString> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_os_string()),
            _ => None,
        })
        .collect();
    let mut cur = PathBuf::new();
    let mut hops = 0u32;
    while let Some(comp) = pending.pop_front() {
        cur.push(&comp);
        if let Some(target) = symlinks.links.get(&cur) {
            hops += 1;
            anyhow::ensure!(hops <= 32, "符号链接层级过深：{:?}", rel);
            let parent = cur.parent().unwrap_or_else(|| Path::new(""));
            let normalized = normalize_symlink_target(parent, target)
                .ok_or_else(|| anyhow::anyhow!("符号链接目标越出 rootfs：{:?}", target))?;
            cur.clear();
            for component in normalized.components().rev() {
                if let Component::Normal(name) = component {
                    pending.push_front(name.to_os_string());
                }
            }
            continue;
        }

        let abs = root.join(&cur);
        if let Ok(md) = std::fs::symlink_metadata(&abs) {
            if md.file_type().is_symlink() {
                hops += 1;
                anyhow::ensure!(hops <= 32, "符号链接层级过深：{:?}", rel);
                let target = std::fs::read_link(&abs)?;
                let parent = cur.parent().unwrap_or_else(|| Path::new(""));
                let normalized = normalize_symlink_target(parent, &target)
                    .ok_or_else(|| anyhow::anyhow!("符号链接目标越出 rootfs：{:?}", target))?;
                cur.clear();
                for component in normalized.components().rev() {
                    if let Component::Normal(name) = component {
                        pending.push_front(name.to_os_string());
                    }
                }
            }
        }
    }
    Ok(root.join(cur))
}

/// symlink 降级：把符号链接物化为目标内容的副本（Windows 无
/// SeCreateSymbolicLinkPrivilege 时 symlink 创建必败，全部层结束后统一复制）。
/// `target_rel_raw` 为条目里写的目标（相对 symlink 所在目录）。
fn materialize_symlink_as_copy(
    root: &Path,
    link_rel: &Path,
    target_raw: &Path,
    symlinks: &SymlinkState,
) -> anyhow::Result<()> {
    let parent_rel = link_rel.parent().unwrap_or_else(|| Path::new(""));
    let norm = normalize_symlink_target(parent_rel, target_raw)
        .ok_or_else(|| anyhow::anyhow!("符号链接目标越出 rootfs：{:?}", target_raw))?;
    let src = resolve_with_symlinks(root, &norm, symlinks)?;
    let dst = root.join(link_rel);
    remove_path(&dst)?;
    if src.is_dir() {
        copy_dir_recursive(&src, &dst)?;
    } else {
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(&src, &dst).map_err(|e| {
            anyhow::anyhow!("symlink 降级复制失败 {:?} <- {:?}: {}", link_rel, src, e)
        })?;
    }
    Ok(())
}

fn remove_path(path: &Path) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(md) if md.is_dir() && !md.file_type().is_symlink() => std::fs::remove_dir_all(path)?,
        Ok(_) => std::fs::remove_file(path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let s = e.path();
        let d = dst.join(e.file_name());
        if s.is_dir() {
            copy_dir_recursive(&s, &d)?;
        } else {
            std::fs::copy(&s, &d)?;
        }
    }
    Ok(())
}

fn finalize_symlinks(root: &Path, symlinks: &SymlinkState) -> anyhow::Result<()> {
    for link_rel in symlinks.links.keys() {
        let dst = root.join(link_rel);
        remove_path(&dst)?;
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }

    #[cfg(unix)]
    for (link_rel, target_raw) in &symlinks.links {
        let dst = root.join(link_rel);
        std::os::unix::fs::symlink(target_raw, &dst)?;
    }

    #[cfg(windows)]
    {
        let mut files = Vec::new();
        let mut directories = Vec::new();
        let mut dangling = Vec::new();
        for (link_rel, target_raw) in &symlinks.links {
            let parent = link_rel.parent().unwrap_or_else(|| Path::new(""));
            let target_rel = normalize_symlink_target(parent, target_raw)
                .ok_or_else(|| anyhow::anyhow!("符号链接目标越出 rootfs：{:?}", target_raw))?;
            let target = resolve_with_symlinks(root, &target_rel, symlinks)?;
            if target.is_dir() {
                directories.push((link_rel, target_raw));
            } else if target.is_file() {
                files.push((link_rel, target_raw));
            } else {
                dangling.push((link_rel, target_raw));
            }
        }

        // 目录目标中可能还包含待落地的文件链接（Debian 的
        // /lib64 -> /usr/lib64 -> /lib/.../ld-linux）。先实现文件，
        // 再从深到浅复制目录，外层目录快照才能包含最终内容。
        for (link_rel, target_raw) in files {
            materialize_symlink_as_copy(root, link_rel, target_raw, symlinks)?;
        }
        directories.sort_by_key(|(link, _)| std::cmp::Reverse(link.components().count()));
        for (link_rel, target_raw) in directories {
            materialize_symlink_as_copy(root, link_rel, target_raw, symlinks)?;
        }
        for (link_rel, target_raw) in dangling {
            if let Err(e) = materialize_symlink_as_copy(root, link_rel, target_raw, symlinks) {
                eprintln!("wbox: 警告：符号链接 {:?} 降级复制失败：{}", link_rel, e);
            }
        }
    }
    Ok(())
}

/// 解包单层到 dest，处理 whiteout、硬链接与符号链接。
/// `media_type` 为 manifest 中该层的 mediaType（用于显式判断压缩格式）。
///
/// 安全防护：
/// - 语法级：拒绝含 `..` / 根 / 前缀成分的条目名与硬链接目标；
/// - 语义级（C1）：所有写出/删除路径先经 `resolve_in_scope` 逐段解析，
///   任何中间组件是越出 rootfs 的 symlink 即跳过；symlink 条目目标本身
///   也须解析在 rootfs 内，否则不创建。
/// - H3：symlink 在所有层应用期间保存在逻辑链接表中，最终才落地；Windows
///   无 SeCreateSymbolicLinkPrivilege 时把最终目标内容复制到链接位置。
#[cfg(test)]
fn unpack_layer(blob: &[u8], dest: &Path, media_type: &str) -> anyhow::Result<()> {
    let mut symlinks = SymlinkState::default();
    unpack_layer_with_state(blob, dest, media_type, &mut symlinks)?;
    finalize_symlinks(dest, &symlinks)
}

fn unpack_layer_with_state(
    blob: &[u8],
    dest: &Path,
    media_type: &str,
    symlinks: &mut SymlinkState,
) -> anyhow::Result<()> {
    let tar_bytes = decompress_layer(blob, media_type)?;

    // ---- 第一遍：收集本层 opaque 目录集合，并在解包任何条目之前应用清理（L7），
    // 避免 opq 条目排在同层新文件之后时误删同层先前条目。
    {
        let mut archive = tar::Archive::new(&tar_bytes[..]);
        let mut opq_dirs: Vec<PathBuf> = Vec::new();
        for entry in archive.entries()? {
            let entry = entry?;
            let path = entry.path()?.into_owned();
            let comps: Vec<_> = path.components().collect();
            if comps.iter().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            }) {
                continue;
            }
            if matches!(comps.last(), Some(std::path::Component::Normal(n)) if *n == ".wh..wh..opq")
            {
                let parent_rel: PathBuf = comps[..comps.len().saturating_sub(1)]
                    .iter()
                    .filter_map(|c| match c {
                        std::path::Component::Normal(n) => Some(n),
                        _ => None,
                    })
                    .collect();
                opq_dirs.push(parent_rel);
            }
        }
        for parent_rel in opq_dirs {
            symlinks.remove_below(&parent_rel);
            let parent_abs = match resolve_with_symlinks(dest, &parent_rel, symlinks) {
                Ok(p) => p,
                Err(_) => continue, // 越界路径：不清理
            };
            if parent_abs.is_dir() {
                for e in std::fs::read_dir(&parent_abs)? {
                    let e = e?;
                    let p = e.path();
                    // 注意不跟随 symlink 判断目录，避免误删链接目标
                    let ft = e.file_type()?;
                    if ft.is_dir() && !ft.is_symlink() {
                        std::fs::remove_dir_all(&p)?;
                    } else {
                        std::fs::remove_file(&p)?;
                    }
                }
            }
        }
    }

    // ---- 第二遍：正式解包 ----
    let mut archive = tar::Archive::new(&tar_bytes[..]);
    // 硬链接目标可能在其链接条目之后才出现（如 ubuntu 层），
    // 先记录、解包完成后再统一创建：(链接路径, 目标路径)，均为 rootfs 相对路径。
    let mut hardlinks: Vec<(PathBuf, PathBuf)> = Vec::new();

    // 单遍流式处理：遇到 whiteout 立即删除目标（规范保证 whiteout 先于目标出现）。
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let comps: Vec<_> = path.components().collect();
        // 路径穿越防护：拒绝含 .. 或根/前缀成分的条目
        if comps.iter().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        }) {
            continue;
        }
        let file_name = match comps.last().and_then(|c| match c {
            std::path::Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        }) {
            Some(n) => n,
            None => continue,
        };
        // 条目所在目录（相对 rootfs）
        let parent_rel: PathBuf = comps[..comps.len().saturating_sub(1)]
            .iter()
            .filter_map(|c| match c {
                std::path::Component::Normal(n) => Some(n),
                _ => None,
            })
            .collect();
        // 父目录逐段解析；中间组件若是越界 symlink 则整个条目跳过（C1）
        let parent_abs = match resolve_with_symlinks(dest, &parent_rel, symlinks) {
            Ok(p) => p,
            Err(_) => continue,
        };

        if file_name == ".wh..wh..opq" {
            // opaque 已在解包前统一应用（L7），此处不落盘。
            continue;
        }
        if let Some(target) = file_name.strip_prefix(".wh.") {
            // whiteout：删除下层同名文件/目录（不跟随 symlink 判定，避免误删链接目标）
            symlinks.remove_at_or_below(&parent_rel.join(target));
            let target_path = parent_abs.join(target);
            match std::fs::symlink_metadata(&target_path) {
                Ok(md) if md.is_dir() && !md.file_type().is_symlink() => {
                    std::fs::remove_dir_all(&target_path).ok();
                }
                Ok(_) => {
                    std::fs::remove_file(&target_path).ok();
                }
                Err(_) => {}
            }
            continue;
        }

        let entry_type = entry.header().entry_type();

        // 硬链接：延迟创建（目标可能尚未解包）
        if entry_type == tar::EntryType::Link {
            if let Ok(Some(target)) = entry.link_name() {
                let tcomps: Vec<_> = target.components().collect();
                if !tcomps.iter().any(|c| {
                    matches!(
                        c,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                }) {
                    hardlinks.push((path.to_path_buf(), target.to_path_buf()));
                }
            }
            continue;
        }

        // 符号链接先进入跨层逻辑表。等所有层完成后再统一落地，避免 Windows
        // 的复制降级成为旧快照，也让 dangling 目标有机会在后续层出现。
        if entry_type == tar::EntryType::Symlink {
            let target = match entry.link_name() {
                Ok(Some(t)) => t.into_owned(),
                _ => continue,
            };
            if !symlink_target_in_scope(&parent_rel, &target) {
                continue; // 绝对目标或 .. 逃逸：拒绝创建
            }
            let out = parent_abs.join(&file_name);
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p)?;
            }
            remove_path(&out)?;
            symlinks.remove_at_or_below(&path);
            symlinks.links.insert(path.to_path_buf(), target);
            continue;
        }

        // 普通条目：解包到解析后的路径（覆盖已存在文件）
        symlinks.remove_at_or_below(&path);
        let out = parent_abs.join(&file_name);
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        entry.unpack(&out)?;
    }

    // 第三遍：创建硬链接；失败时（如文件系统不支持）退化为复制。
    for (link_rel, target_rel) in hardlinks {
        symlinks.remove_at_or_below(&link_rel);
        let link_abs = match resolve_with_symlinks(dest, &link_rel, symlinks) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let target_abs = match resolve_with_symlinks(dest, &target_rel, symlinks) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if let Some(p) = link_abs.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::remove_file(&link_abs).ok();
        if std::fs::hard_link(&target_abs, &link_abs).is_err() {
            std::fs::copy(&target_abs, &link_abs)
                .map_err(|e| anyhow::anyhow!("创建硬链接/复制失败 {:?}: {}", link_rel, e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个内存 tar（未压缩），条目为 (路径, 内容)。
    fn make_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (p, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, p, *data).unwrap();
        }
        b.into_inner().unwrap()
    }

    /// 构造含 symlink 条目的内存 tar：(路径, 目标)。
    fn make_tar_with_symlinks<'a>(
        files: &[(&'a str, &'a [u8])],
        symlinks: &[(&'a str, &'a str)],
    ) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (p, data) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, p, *data).unwrap();
        }
        for (p, target) in symlinks {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            h.set_mode(0o777);
            h.set_cksum();
            b.append_link(&mut h, p, target).unwrap();
        }
        b.into_inner().unwrap()
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wbox-test-{}-{}", tag, std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use std::io::Write;
        let mut e = GzEncoder::new(Vec::new(), flate2::Compression::fast());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn failed_cache_swap_restores_old_cache_and_cleans_staging() {
        let dir = tmpdir("pull-rollback");
        let dest = dir.join("image");
        std::fs::create_dir_all(dest.join("rootfs")).unwrap();
        std::fs::write(dest.join("rootfs/marker"), b"old-complete").unwrap();

        let staging = StagingDir::create(&dest).unwrap();
        std::fs::create_dir_all(staging.path.join("rootfs")).unwrap();
        std::fs::write(staging.path.join("rootfs/marker"), b"new-complete").unwrap();
        std::fs::write(staging.path.join("manifest.json"), b"{}").unwrap();
        let staging_path = staging.path.clone();

        let mut rename_call = 0;
        let err = replace_cache_dir_with(&staging.path, &dest, |from, to| {
            rename_call += 1;
            if rename_call == 2 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected install rename failure",
                ));
            }
            std::fs::rename(from, to)
        })
        .unwrap_err();
        assert!(err.to_string().contains("旧缓存已恢复"), "{err}");
        assert_eq!(
            std::fs::read(dest.join("rootfs/marker")).unwrap(),
            b"old-complete"
        );

        // 模拟 pull_image 错误返回：guard 必须清掉未提交的 staging。
        drop(staging);
        assert!(!staging_path.exists(), "失败 staging 不应残留");
        assert!(
            std::fs::read_dir(&dir).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("backup")),
            "成功回滚后不应残留 backup"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn staging_guard_cleans_simulated_download_failure() {
        let dir = tmpdir("pull-staging-cleanup");
        let dest = dir.join("image");
        let staging_path;
        {
            let staging = StagingDir::create(&dest).unwrap();
            staging_path = staging.path.clone();
            std::fs::create_dir_all(staging.path.join("rootfs")).unwrap();
            std::fs::write(staging.path.join("rootfs/partial-layer"), b"partial").unwrap();
        }
        assert!(!staging_path.exists(), "下载失败后必须清理 staging");
        assert!(!dest.exists(), "失败 pull 不得生成目标缓存");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pull_commit_lock_serializes_same_destination() {
        let dir = tmpdir("pull-lock");
        let dest = dir.join("image");
        std::fs::create_dir_all(&dir).unwrap();

        let first = PullCommitLock::acquire(&dest, Duration::from_millis(50)).unwrap();
        let err = PullCommitLock::acquire(&dest, Duration::from_millis(50))
            .err()
            .expect("第二个提交者应等待超时");
        assert!(err.to_string().contains("超时"), "{err}");
        drop(first);
        let second = PullCommitLock::acquire(&dest, Duration::from_millis(50)).unwrap();
        drop(second);
        assert!(
            !sibling_path(&dest, "pull.lock", false).unwrap().exists(),
            "释放提交锁后不得残留锁目录"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn digest_verify_ok_and_fail() {
        let data = b"hello";
        let d = format!("sha256:{}", hex_sha256(data));
        verify_digest(&d, data).unwrap();
        assert!(verify_digest(&d, b"other").is_err());
        assert!(verify_digest("sha512:xx", data).is_err());
    }

    #[test]
    fn unpack_gzip_tar_and_whiteout() {
        let dir = std::env::temp_dir().join(format!("wbox-test-{}", std::process::id()));
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();

        // 第一层：/a.txt 与 /dir/keep.txt、/dir/gone.txt
        let l1 = make_tar(&[
            ("a.txt", b"aaa" as &[u8]),
            ("dir/keep.txt", b"k"),
            ("dir/gone.txt", b"g"),
        ]);
        unpack_layer(
            &gzip(&l1),
            &rootfs,
            "application/vnd.oci.image.layer.v1.tar+gzip",
        )
        .unwrap();
        assert!(rootfs.join("a.txt").exists());
        assert!(rootfs.join("dir/gone.txt").exists());

        // 第二层：whiteout 删 gone.txt，新增 b.txt
        let l2 = make_tar(&[("dir/.wh.gone.txt", b"" as &[u8]), ("b.txt", b"bb")]);
        unpack_layer(
            &gzip(&l2),
            &rootfs,
            "application/vnd.oci.image.layer.v1.tar+gzip",
        )
        .unwrap();
        assert!(!rootfs.join("dir/gone.txt").exists(), "whiteout 未删除目标");
        assert!(
            !rootfs.join("dir/.wh.gone.txt").exists(),
            "whiteout 文件不应落盘"
        );
        assert!(rootfs.join("dir/keep.txt").exists());
        assert_eq!(std::fs::read(rootfs.join("b.txt")).unwrap(), b"bb");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpack_opaque_dir() {
        let dir = std::env::temp_dir().join(format!("wbox-test-opq-{}", std::process::id()));
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(rootfs.join("d/sub")).unwrap();
        std::fs::write(rootfs.join("d/old.txt"), b"x").unwrap();
        std::fs::write(rootfs.join("d/sub/old2.txt"), b"y").unwrap();

        // opaque 层：.wh..wh..opq + 一个新文件
        let l = make_tar(&[
            ("d/.wh..wh..opq", b"" as &[u8]),
            ("d/new.txt", b"n" as &[u8]),
        ]);
        unpack_layer(&l, &rootfs, "application/vnd.oci.image.layer.v1.tar").unwrap();

        assert!(!rootfs.join("d/old.txt").exists());
        assert!(!rootfs.join("d/sub").exists(), "opaque 应清空下层子目录");
        assert!(rootfs.join("d/new.txt").exists());
        assert!(!rootfs.join("d/.wh..wh..opq").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpack_rejects_path_traversal() {
        let dir = std::env::temp_dir().join(format!("wbox-test-trav-{}", std::process::id()));
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        // tar crate 的 Builder 会拒绝 `..`，手动构造 header 字节以模拟恶意归档
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(1);
        h.set_mode(0o644);
        h.as_mut_bytes()[..11].copy_from_slice(b"../evil.txt");
        h.set_cksum();
        b.append(&h, &b"e"[..]).unwrap(); // append 不覆写 header 中的路径字段
        let l = b.into_inner().unwrap();
        unpack_layer(&l, &rootfs, "application/vnd.oci.image.layer.v1.tar").unwrap();
        assert!(!dir.join("evil.txt").exists(), "路径穿越条目应被跳过");
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- C1：symlink 逃逸防护 ----

    #[test]
    fn unpack_rejects_symlink_escape_absolute_target() {
        let dir = tmpdir("symabs");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        // 恶意层：symlink /link -> /tmp（绝对目标），随后写 link/evil.txt
        let l = make_tar_with_symlinks(
            &[("link/evil.txt", b"e" as &[u8])],
            &[("link", dir.to_str().unwrap())],
        );
        unpack_layer(&l, &rootfs, "").unwrap();
        assert!(
            !dir.join("evil.txt").exists(),
            "绝对目标 symlink 应被拒绝，后续条目不得写出 rootfs"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpack_rejects_symlink_escape_dotdot() {
        let dir = tmpdir("symdot");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        // 恶意层：symlink sub/link -> ../../（越出 rootfs），随后写 sub/link/evil.txt
        let l = make_tar_with_symlinks(
            &[("sub/link/evil.txt", b"e" as &[u8])],
            &[("sub/link", "../../")],
        );
        unpack_layer(&l, &rootfs, "").unwrap();
        assert!(
            !dir.join("evil.txt").exists(),
            ".. 逃逸 symlink 应被拒绝，后续条目不得写出 rootfs"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpack_allows_in_scope_symlink() {
        let dir = tmpdir("symok");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        // 合法层：目录 real/ 与指向它的同级 symlink alias -> real，再经 alias 写入
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_mode(0o777);
        h.set_cksum();
        b.append_link(&mut h, "alias", "real").unwrap();
        for (path, data) in [
            ("real/keep.txt", b"k".as_slice()),
            ("alias/new.txt", b"n".as_slice()),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, path, data).unwrap();
        }
        let l2 = b.into_inner().unwrap();
        unpack_layer(&l2, &rootfs, "").unwrap();
        assert!(rootfs.join("real/keep.txt").exists());
        // 经 in-scope symlink 写入的文件应落在 rootfs 内的真实目录
        assert!(
            rootfs.join("real/new.txt").exists() || rootfs.join("alias/new.txt").exists(),
            "in-scope symlink 写入应解析到 rootfs 内"
        );
        assert!(!dir.join("new.txt").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpack_absolute_symlink_uses_container_root() {
        let dir = tmpdir("symtgt");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(rootfs.join("etc")).unwrap();
        std::fs::write(rootfs.join("etc/value"), b"container").unwrap();
        let l = make_tar_with_symlinks(&[], &[("alias", "/etc")]);
        unpack_layer(&l, &rootfs, "").unwrap();
        if cfg!(windows) {
            assert_eq!(
                std::fs::read(rootfs.join("alias/value")).unwrap(),
                b"container"
            );
            assert!(rootfs.join("alias").is_dir());
        } else {
            assert_eq!(
                std::fs::read_link(rootfs.join("alias")).unwrap(),
                Path::new("/etc")
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- H2：digest 链——按 digest 取回的 body 必须过 verify_digest ----

    #[test]
    fn digest_chain_manifest_bodies_verified() {
        // 模拟 index 与子 manifest 两级 body：按各自 digest 校验应通过，
        // 篡改后的 body 必须被拒绝（即拉取路径上任何按 digest 取回的 body
        // 都无法绕过 verify_digest）。
        let index_body = br#"{"manifests":[{"digest":"sha256:deadbeef"}]}"#;
        let child_body = br#"{"schemaVersion":2,"layers":[]}"#;
        let index_d = sha256_hex_prefixed(index_body);
        let child_d = sha256_hex_prefixed(child_body);
        verify_digest(&index_d, index_body).unwrap();
        verify_digest(&child_d, child_body).unwrap();
        // 篡改 index 本体
        let mut evil_index = index_body.to_vec();
        evil_index[20] ^= 0xff;
        assert!(
            verify_digest(&index_d, &evil_index).is_err(),
            "index 本体必须校验"
        );
        // 篡改子 manifest
        let mut evil_child = child_body.to_vec();
        evil_child[5] ^= 0xff;
        assert!(
            verify_digest(&child_d, &evil_child).is_err(),
            "子 manifest 必须校验"
        );
    }

    // ---- H3：symlink 降级为复制 ----

    #[test]
    fn symlink_fallback_copies_target_content() {
        let dir = tmpdir("symfb");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(rootfs.join("usr/bin")).unwrap();
        std::fs::write(rootfs.join("usr/bin/real"), b"binary").unwrap();
        let symlinks = SymlinkState::default();
        // 直接调用降级路径（模拟 Windows 上 symlink 创建失败后的层末物化）
        materialize_symlink_as_copy(
            &rootfs,
            Path::new("usr/bin/alias"),
            Path::new("real"),
            &symlinks,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(rootfs.join("usr/bin/alias")).unwrap(),
            b"binary"
        );
        // 目录目标的递归复制
        materialize_symlink_as_copy(
            &rootfs,
            Path::new("usr/libcopy"),
            Path::new("bin"),
            &symlinks,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(rootfs.join("usr/libcopy/real")).unwrap(),
            b"binary"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cross_layer_symlink_observes_later_target_update() {
        let dir = tmpdir("sym-cross-layer");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let mut symlinks = SymlinkState::default();

        let first =
            make_tar_with_symlinks(&[("real/value", b"old" as &[u8])], &[("alias", "real")]);
        unpack_layer_with_state(&first, &rootfs, "", &mut symlinks).unwrap();
        let second = make_tar(&[("real/value", b"new" as &[u8])]);
        unpack_layer_with_state(&second, &rootfs, "", &mut symlinks).unwrap();
        finalize_symlinks(&rootfs, &symlinks).unwrap();

        assert_eq!(
            std::fs::read(rootfs.join("alias/value")).unwrap(),
            b"new",
            "链接必须看到后续层对目标目录的最终修改"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dangling_symlink_waits_for_target_in_later_layer() {
        let dir = tmpdir("sym-dangling-layer");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let mut symlinks = SymlinkState::default();

        let first = make_tar_with_symlinks(&[], &[("bin/tool", "../lib/tool")]);
        unpack_layer_with_state(&first, &rootfs, "", &mut symlinks).unwrap();
        assert!(
            symlinks.links.contains_key(Path::new("bin/tool")),
            "dangling 链接不能在目标出现前丢失"
        );
        let second = make_tar(&[("lib/tool", b"ready" as &[u8])]);
        unpack_layer_with_state(&second, &rootfs, "", &mut symlinks).unwrap();
        finalize_symlinks(&rootfs, &symlinks).unwrap();

        assert_eq!(
            std::fs::read(rootfs.join("bin/tool")).unwrap(),
            b"ready",
            "后续层创建目标后必须恢复链接语义"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpack_symlink_after_target_in_later_entry() {
        // symlink 目标确实排在同层后续条目；层末降级仍应解析到目标。
        let dir = tmpdir("symorder");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_mode(0o777);
        h.set_cksum();
        b.append_link(&mut h, "link", "/target.txt").unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_size(1);
        h.set_mode(0o755);
        h.set_cksum();
        b.append_data(&mut h, "target.txt", &b"t"[..]).unwrap();
        let l = b.into_inner().unwrap();
        unpack_layer(&l, &rootfs, "").unwrap();
        if cfg!(windows) {
            assert_eq!(std::fs::read(rootfs.join("link")).unwrap(), b"t");
        } else {
            assert_eq!(
                std::fs::read_link(rootfs.join("link")).unwrap(),
                Path::new("/target.txt")
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpack_alpine_busybox_absolute_applets() {
        let dir = tmpdir("alpine-busybox");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();

        // Alpine 3.20 的层以绝对 symlink 安装 applet；ash 在 busybox
        // 本体之前，cat/sh 在其后。Windows 上都必须解析到容器根。
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_mode(0o777);
        h.set_cksum();
        b.append_link(&mut h, "bin/ash", "/bin/busybox").unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_size(7);
        h.set_mode(0o755);
        h.set_cksum();
        b.append_data(&mut h, "bin/busybox", &b"busybox"[..])
            .unwrap();
        for applet in ["bin/cat", "bin/sh"] {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            h.set_mode(0o777);
            h.set_cksum();
            b.append_link(&mut h, applet, "/bin/busybox").unwrap();
        }
        let l = b.into_inner().unwrap();

        unpack_layer(&l, &rootfs, "").unwrap();
        for applet in ["bin/ash", "bin/cat", "bin/sh"] {
            if cfg!(windows) {
                assert_eq!(
                    std::fs::read(rootfs.join(applet)).unwrap(),
                    b"busybox",
                    "{applet} 应物化为容器内 /bin/busybox"
                );
            } else {
                assert_eq!(
                    std::fs::read_link(rootfs.join(applet)).unwrap(),
                    Path::new("/bin/busybox")
                );
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpack_debian_loader_absolute_directory_symlinks() {
        let dir = tmpdir("debian-loader-links");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();

        // bookworm-slim 的目录链接与动态加载器链接都可能先于最终目标出现。
        let mut b = tar::Builder::new(Vec::new());
        for (link, target) in [
            ("lib64", "/usr/lib64"),
            (
                "usr/lib64/ld-linux-x86-64.so.2",
                "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
            ),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            h.set_mode(0o777);
            h.set_cksum();
            b.append_link(&mut h, link, target).unwrap();
        }
        let loader = b"elf-loader";
        let mut h = tar::Header::new_gnu();
        h.set_size(loader.len() as u64);
        h.set_mode(0o755);
        h.set_cksum();
        b.append_data(
            &mut h,
            "lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
            loader.as_slice(),
        )
        .unwrap();

        unpack_layer(&b.into_inner().unwrap(), &rootfs, "").unwrap();
        if cfg!(windows) {
            assert_eq!(
                std::fs::read(rootfs.join("usr/lib64/ld-linux-x86-64.so.2")).unwrap(),
                loader
            );
            assert_eq!(
                std::fs::read(rootfs.join("lib64/ld-linux-x86-64.so.2")).unwrap(),
                loader,
                "外层目录链接必须在内部文件链接落地后复制"
            );
        } else {
            assert_eq!(
                std::fs::read_link(rootfs.join("lib64")).unwrap(),
                Path::new("/usr/lib64")
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- L7：opaque 目录在解包本层条目之前应用 ----

    #[test]
    fn unpack_opaque_applied_before_same_layer_entries() {
        let dir = tmpdir("opqorder");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(rootfs.join("d")).unwrap();
        std::fs::write(rootfs.join("d/old.txt"), b"x").unwrap();
        // opq 条目排在新文件之后（规范不禁止的顺序）：
        // 旧实现会误删同层先前写入的 new.txt；修复后应先清下层再解包。
        let l = make_tar(&[
            ("d/new.txt", b"n" as &[u8]),
            ("d/.wh..wh..opq", b"" as &[u8]),
        ]);
        unpack_layer(&l, &rootfs, "").unwrap();
        assert!(!rootfs.join("d/old.txt").exists(), "opaque 应清空下层内容");
        assert!(
            rootfs.join("d/new.txt").exists(),
            "同层新文件不应被 opaque 误删"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- M6：压缩格式显式判断 ----

    #[test]
    fn rejects_zstd_layer() {
        let dir = tmpdir("zstd");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let fake_zstd = [0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x01];
        // 按 mediaType 显式拒绝
        let e = unpack_layer(
            &fake_zstd,
            &rootfs,
            "application/vnd.oci.image.layer.v1.tar+zstd",
        )
        .unwrap_err();
        assert!(e.to_string().contains("不支持的压缩格式"), "{}", e);
        // 空 mediaType 时按 magic 嗅探拒绝
        let e = unpack_layer(&fake_zstd, &rootfs, "").unwrap_err();
        assert!(e.to_string().contains("不支持的压缩格式"), "{}", e);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn plain_tar_media_type_supported() {
        let dir = tmpdir("plaintar");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let l = make_tar(&[("x.txt", b"x" as &[u8])]);
        unpack_layer(&l, &rootfs, "application/vnd.oci.image.layer.v1.tar").unwrap();
        unpack_layer(&l, &rootfs, "application/vnd.docker.image.rootfs.diff.tar").unwrap();
        assert!(rootfs.join("x.txt").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- digest 校验失败路径细分 ----

    #[test]
    fn verify_digest_rejects_unsupported_algorithm() {
        let e = verify_digest("sha512:abcd", b"x").unwrap_err();
        assert!(e.to_string().contains("不支持的 digest 算法"), "{}", e);
        // 无算法前缀
        assert!(verify_digest("deadbeef", b"x").is_err());
        // 空 hex 段（sha256: 后为空）→ 不匹配
        assert!(verify_digest("sha256:", b"x").is_err());
    }

    #[test]
    fn verify_digest_mismatch_reports_expected_and_actual() {
        let e = verify_digest(&sha256_hex_prefixed(b"a"), b"b").unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("digest 不匹配"), "{}", msg);
        assert!(msg.contains(&hex_sha256(b"a")), "期望含 expected：{}", msg);
        assert!(msg.contains(&hex_sha256(b"b")), "期望含 actual：{}", msg);
    }

    // ---- select_manifest ----

    fn index_with(platforms: &[(&str, &str, &str)]) -> serde_json::Value {
        let manifests: Vec<serde_json::Value> = platforms
            .iter()
            .map(|(os, arch, digest)| {
                serde_json::json!({
                    "digest": digest,
                    "platform": {"os": os, "architecture": arch}
                })
            })
            .collect();
        serde_json::json!({"manifests": manifests})
    }

    #[test]
    fn select_manifest_picks_matching_platform() {
        let idx = index_with(&[
            ("linux", "arm64", "sha256:arm"),
            ("linux", "amd64", "sha256:amd"),
            ("windows", "amd64", "sha256:win"),
        ]);
        assert_eq!(
            select_manifest(&idx, "linux", "amd64").unwrap(),
            "sha256:amd"
        );
        assert_eq!(
            select_manifest(&idx, "linux", "arm64").unwrap(),
            "sha256:arm"
        );
    }

    #[test]
    fn select_manifest_skips_attestation_entries() {
        // attestations 条目 platform 为 unknown/unknown：不得匹配
        let idx = index_with(&[
            ("unknown", "unknown", "sha256:att"),
            ("linux", "amd64", "sha256:real"),
        ]);
        assert_eq!(
            select_manifest(&idx, "linux", "amd64").unwrap(),
            "sha256:real"
        );
        // 全部 unknown 平台时查询 unknown/unknown 会命中第一条（记录现状）
        let idx = index_with(&[("unknown", "unknown", "sha256:att")]);
        assert_eq!(
            select_manifest(&idx, "unknown", "unknown").unwrap(),
            "sha256:att"
        );
    }

    #[test]
    fn select_manifest_no_match_lists_available_platforms() {
        let idx = index_with(&[
            ("linux", "arm64", "sha256:arm"),
            ("windows", "amd64", "sha256:w"),
        ]);
        let e = select_manifest(&idx, "linux", "amd64").unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("linux/amd64"), "{}", msg);
        assert!(
            msg.contains("linux/arm64") && msg.contains("windows/amd64"),
            "{}",
            msg
        );
    }

    #[test]
    fn select_manifest_missing_manifests_array() {
        let e = select_manifest(&serde_json::json!({"schemaVersion": 2}), "linux", "amd64")
            .unwrap_err();
        assert!(e.to_string().contains("manifests"), "{}", e);
    }

    #[test]
    fn select_manifest_match_without_digest_is_error() {
        let idx = serde_json::json!({"manifests": [{"platform": {"os": "linux", "architecture": "amd64"}}]});
        let e = select_manifest(&idx, "linux", "amd64").unwrap_err();
        assert!(e.to_string().contains("digest"), "{}", e);
    }

    // ---- 压缩格式嗅探 ----

    #[test]
    fn magic_sniffing_helpers() {
        let gz = gzip(b"abc");
        assert!(is_gzip(&gz) && !is_zstd(&gz));
        assert!(!is_gzip(b"\x1f")); // 太短
        assert!(!is_gzip(b""));
        assert!(is_zstd(&[0x28, 0xb5, 0x2f, 0xfd]));
        assert!(!is_zstd(&[0x28, 0xb5, 0x2f])); // 太短
    }

    #[test]
    fn decompress_layer_format_dispatch() {
        let tar = make_tar(&[("f", b"x" as &[u8])]);
        let gz = gzip(&tar);
        // gzip mediaType（大小写不敏感）
        assert_eq!(
            decompress_layer(&gz, "application/vnd.oci.image.layer.v1.tar+gzip").unwrap(),
            tar
        );
        assert_eq!(
            decompress_layer(&gz, "Application/Vnd.Oci.Image.Layer.V1.Tar+Gzip").unwrap(),
            tar
        );
        // 空 mediaType 按 magic 嗅探 gzip
        assert_eq!(decompress_layer(&gz, "").unwrap(), tar);
        // 未知格式显式拒绝（mediaType 不含 tar/layer/gzip/zstd 关键词）
        let e = decompress_layer(&tar, "application/x-brotli").unwrap_err();
        assert!(e.to_string().contains("不支持的"), "{}", e);
        // 记录现状：mediaType 含 "layer" 的未知压缩（如 ...layer.v1.brotli）
        // 会被当未压缩 tar 放行——宽松匹配的历史行为，由 digest 校验兜底完整性。
        // zstd mediaType（大小写不敏感）
        assert!(decompress_layer(&tar, "x+zstd").is_err());
        assert!(decompress_layer(&tar, "x+ZSTD").is_err());
    }

    #[test]
    fn decompress_rejects_truncated_gzip() {
        let gz = gzip(b"hello");
        let truncated = &gz[..gz.len() / 2];
        assert!(decompress_layer(truncated, "application/x+gzip").is_err());
    }

    // ---- index media type 判定 ----

    #[test]
    fn is_index_media_types() {
        assert!(is_index("application/vnd.oci.image.index.v1+json"));
        assert!(is_index(
            "application/vnd.docker.distribution.manifest.list.v2+json"
        ));
        assert!(!is_index("application/vnd.oci.image.manifest.v1+json"));
        assert!(!is_index(
            "application/vnd.docker.distribution.manifest.v2+json"
        ));
        assert!(!is_index(""));
    }

    // ---- 硬链接与 whiteout 目录删除 ----

    #[test]
    fn unpack_hardlink_created_with_same_content() {
        let dir = tmpdir("hardlink");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        // 硬链接排在目标之前，验证延迟创建与不支持 hardlink 时的复制 fallback。
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Link);
        h.set_size(0);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_link(&mut h, "bin/alias", "bin/real").unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_size(5);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "bin/real", &b"hello"[..]).unwrap();
        let l = b.into_inner().unwrap();
        unpack_layer(&l, &rootfs, "").unwrap();
        assert_eq!(std::fs::read(rootfs.join("bin/alias")).unwrap(), b"hello");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpack_hardlink_with_traversal_target_rejected() {
        let dir = tmpdir("hardlink-trav");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Link);
        h.set_size(0);
        h.set_cksum();
        b.append_link(&mut h, "evil", "../../etc/passwd").unwrap();
        let l = b.into_inner().unwrap();
        unpack_layer(&l, &rootfs, "").unwrap();
        assert!(!rootfs.join("evil").exists(), "穿越目标硬链接不得创建");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn whiteout_removes_directory_recursively() {
        let dir = tmpdir("whdir");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(rootfs.join("d/sub")).unwrap();
        std::fs::write(rootfs.join("d/sub/f.txt"), b"x").unwrap();
        let l = make_tar(&[(".wh.d", b"" as &[u8])]);
        unpack_layer(&l, &rootfs, "").unwrap();
        assert!(!rootfs.join("d").exists(), "whiteout 应递归删除目录");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn whiteout_nonexistent_target_is_noop() {
        let dir = tmpdir("whnoop");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let l = make_tar(&[
            (".wh.never-existed", b"" as &[u8]),
            ("ok.txt", b"o" as &[u8]),
        ]);
        unpack_layer(&l, &rootfs, "").unwrap();
        assert!(rootfs.join("ok.txt").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- symlink_target_in_scope 单元覆盖 ----

    #[test]
    fn symlink_target_scope_rules() {
        use std::path::Path;
        // 相对目标在同层/子层：允许
        assert!(symlink_target_in_scope(
            Path::new("usr/bin"),
            Path::new("real")
        ));
        assert!(symlink_target_in_scope(
            Path::new("usr/bin"),
            Path::new("../lib/x")
        ));
        // 根级 symlink 的 ../x：弹出根 → 拒绝
        assert!(!symlink_target_in_scope(Path::new(""), Path::new("../x")));
        // 越出 rootfs 的 ..：拒绝
        assert!(!symlink_target_in_scope(
            Path::new("a"),
            Path::new("../../x")
        ));
        // Linux 绝对目标以容器根解析，而不是宿主根。
        assert!(symlink_target_in_scope(
            Path::new("a"),
            Path::new("/etc/passwd")
        ));
        assert_eq!(
            normalize_symlink_target(Path::new("a"), Path::new("/etc/passwd")).unwrap(),
            Path::new("etc/passwd")
        );
        // CurDir 组件无影响
        assert!(symlink_target_in_scope(Path::new("a"), Path::new("./b")));
    }

    // ---- gzip + whiteout 组合、嵌套路径 ----

    #[test]
    fn unpack_nested_whiteout_in_subdir() {
        let dir = tmpdir("whnest");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(rootfs.join("a/b")).unwrap();
        std::fs::write(rootfs.join("a/b/gone"), b"g").unwrap();
        std::fs::write(rootfs.join("a/keep"), b"k").unwrap();
        let l = make_tar(&[("a/b/.wh.gone", b"" as &[u8])]);
        unpack_layer(
            &gzip(&l),
            &rootfs,
            "application/vnd.oci.image.layer.v1.tar+gzip",
        )
        .unwrap();
        assert!(!rootfs.join("a/b/gone").exists());
        assert!(rootfs.join("a/keep").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
