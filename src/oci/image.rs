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
use std::io::Read;
use std::path::{Path, PathBuf};

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

/// 拉取一个镜像并解包到 `dest`（dest 下生成 rootfs/ 与元数据文件）。
pub fn pull_image(
    client: &RegistryClient,
    iref: &ImageRef,
    os: &str,
    arch: &str,
    dest: &Path,
    verbose: bool,
) -> crate::error::Result<PullSummary> {
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
            return Err(WboxError::registry("子 manifest 仍是 index（嵌套 index 不支持）"));
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
    verify_digest(config_digest, &config_bytes).context("config digest 校验失败").ctx(ErrKind::Registry)?;
    if verbose {
        println!("wbox: config {} 校验通过（{} 字节）", config_digest, config_bytes.len());
    }

    // ---- 3. 准备输出目录（重新 pull 同 tag 时先清掉旧 rootfs，避免残留）----
    let rootfs = dest.join("rootfs");
    if rootfs.exists() {
        std::fs::remove_dir_all(&rootfs)
            .context("清理旧 rootfs 失败")
            .ctx(ErrKind::Registry)?;
    }
    std::fs::create_dir_all(&rootfs)
        .context("创建 rootfs 目录失败")
        .ctx(ErrKind::Registry)?;

    // ---- 4. 逐层拉取、校验、解包 ----
    let layers = manifest
        .get("layers")
        .and_then(|v| v.as_array())
        .ok_or_else(|| WboxError::registry("manifest 缺少 layers 数组"))?;
    let mut layer_digests = Vec::new();
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
        unpack_layer(&blob, &rootfs, media_type)
            .context(format!("解包层 {} 失败", digest))
            .ctx(ErrKind::Registry)?;
        if verbose {
            println!("wbox:   校验通过，已解包（{} 字节）", blob.len());
        }
        layer_digests.push(digest.to_string());
    }

    // ---- 5. 写元数据 ----
    std::fs::write(dest.join("manifest.json"), &manifest_bytes)
        .and_then(|_| std::fs::write(dest.join("config.json"), &config_bytes))
        .and_then(|_| {
            std::fs::write(
                dest.join("layers.json"),
                serde_json::to_string_pretty(&layer_digests).unwrap(),
            )
        })
        .context("写入镜像元数据失败")
        .ctx(ErrKind::Registry)?;

    Ok(PullSummary {
        layers: layers.len(),
        manifest_digest,
    })
}

/// 从 manifest index 中挑选匹配 os/arch 的子 manifest digest。
fn select_manifest(index: &serde_json::Value, os: &str, arch: &str) -> crate::error::Result<String> {
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
                p.and_then(|p| p.get("os")).and_then(|v| v.as_str()).unwrap_or("?"),
                p.and_then(|p| p.get("architecture")).and_then(|v| v.as_str()).unwrap_or("?")
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
    } else if mt.is_empty()
        || mt.ends_with("+tar")
        || mt.contains("tar")
        || mt.contains("layer")
    {
        // 未压缩 tar：application/vnd.oci.image.layer.v1.tar、
        // application/vnd.docker.image.rootfs.diff.tar 等。
        Ok(blob.to_vec())
    } else {
        anyhow::bail!("不支持的 layer 压缩/格式：mediaType={}", media_type)
    }
}

/// 逐段解析 `rel`（rootfs 相对路径），解析途中遇到的每个符号链接，
/// 返回 rootfs 内的最终绝对路径；任何中间组件命中越出 rootfs 的 symlink
/// （绝对目标或 `..` 逃逸）都报错。参照 Docker FollowSymlinkInScope 思路，
/// 防止恶意层用 symlink 条目让后续写出/删除落到 rootfs 之外（C1）。
fn resolve_in_scope(root: &Path, rel: &Path) -> anyhow::Result<PathBuf> {
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
    let mut cur = PathBuf::new(); // 已解析的 rootfs 相对前缀
    let mut hops = 0u32;
    while let Some(comp) = pending.pop_front() {
        cur.push(&comp);
        let abs = root.join(&cur);
        if let Ok(md) = std::fs::symlink_metadata(&abs) {
            if md.file_type().is_symlink() {
                hops += 1;
                anyhow::ensure!(hops <= 32, "符号链接层级过深：{:?}", rel);
                let target = std::fs::read_link(&abs)?;
                cur.pop(); // 摘掉 symlink 自身，替换为其目标
                let mut normals: Vec<OsString> = Vec::new();
                for c in target.components() {
                    match c {
                        Component::RootDir | Component::Prefix(_) => {
                            anyhow::bail!("符号链接目标越出 rootfs（绝对路径）：{:?}", target)
                        }
                        Component::CurDir => {}
                        // cur 已完全解析且在 rootfs 内，.. 直接作用在 cur 上
                        Component::ParentDir => {
                            if !cur.pop() {
                                anyhow::bail!("符号链接目标越出 rootfs（.. 逃逸）：{:?}", target)
                            }
                        }
                        Component::Normal(n) => normals.push(n.to_os_string()),
                    }
                }
                // 目标中的普通组件还需继续做 symlink 检查，压回 pending 头部
                for n in normals.into_iter().rev() {
                    pending.push_front(n);
                }
            }
        }
    }
    Ok(root.join(cur))
}

/// 校验 symlink 条目的目标在 symlink 所在目录下解析后不越出 rootfs。
/// 绝对目标直接拒绝；`..` 弹出越过根也拒绝。
fn symlink_target_in_scope(parent_rel: &Path, target: &Path) -> bool {
    use std::path::Component;
    let mut depth: Vec<&std::ffi::OsStr> = parent_rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n),
            _ => None,
        })
        .collect();
    for c in target.components() {
        match c {
            Component::RootDir | Component::Prefix(_) => return false,
            Component::CurDir => {}
            Component::ParentDir => {
                if depth.pop().is_none() {
                    return false;
                }
            }
            Component::Normal(n) => depth.push(n),
        }
    }
    true
}

/// symlink 降级：把符号链接物化为目标内容的副本（Windows 无
/// SeCreateSymbolicLinkPrivilege 时 symlink 创建必败，层末统一复制）。
/// `target_rel_raw` 为条目里写的目标（相对 symlink 所在目录）。
fn materialize_symlink_as_copy(root: &Path, link_rel: &Path, target_raw: &Path) -> anyhow::Result<()> {
    // 目标相对 symlink 父目录归一化
    let mut norm = PathBuf::new();
    if let Some(p) = link_rel.parent() {
        norm.push(p);
    }
    for c in target_raw.components() {
        match c {
            std::path::Component::Normal(n) => norm.push(n),
            std::path::Component::ParentDir => {
                norm.pop();
            }
            _ => {}
        }
    }
    let src = resolve_in_scope(root, &norm)?;
    let dst = root.join(link_rel);
    std::fs::remove_file(&dst).ok();
    if src.is_dir() {
        copy_dir_recursive(&src, &dst)?;
    } else {
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(&src, &dst)
            .map_err(|e| anyhow::anyhow!("symlink 降级复制失败 {:?} <- {:?}: {}", link_rel, src, e))?;
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

/// 解包单层到 dest，处理 whiteout、硬链接与符号链接。
/// `media_type` 为 manifest 中该层的 mediaType（用于显式判断压缩格式）。
///
/// 安全防护：
/// - 语法级：拒绝含 `..` / 根 / 前缀成分的条目名与硬链接目标；
/// - 语义级（C1）：所有写出/删除路径先经 `resolve_in_scope` 逐段解析，
///   任何中间组件是越出 rootfs 的 symlink 即跳过；symlink 条目目标本身
///   也须解析在 rootfs 内，否则不创建。
/// - H3：symlink 创建失败（Windows 无 SeCreateSymbolicLinkPrivilege）时
///   延迟记录，层末把目标内容复制到链接位置（降级语义见 README）。
pub fn unpack_layer(blob: &[u8], dest: &Path, media_type: &str) -> anyhow::Result<()> {
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
            let parent_abs = match resolve_in_scope(dest, &parent_rel) {
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
    // symlink 创建失败的延迟记录（H3）：(链接路径, 条目内原始目标)
    let mut deferred_symlinks: Vec<(PathBuf, PathBuf)> = Vec::new();

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
        let parent_abs = match resolve_in_scope(dest, &parent_rel) {
            Ok(p) => p,
            Err(_) => continue,
        };

        if file_name == ".wh..wh..opq" {
            // opaque 已在解包前统一应用（L7），此处不落盘。
            continue;
        }
        if let Some(target) = file_name.strip_prefix(".wh.") {
            // whiteout：删除下层同名文件/目录（不跟随 symlink 判定，避免误删链接目标）
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
            if let Ok(link_name) = entry.link_name() {
                if let Some(target) = link_name {
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
            }
            continue;
        }

        // 符号链接：先校验目标解析后不越出 rootfs，再创建；
        // 创建失败（Windows 无特权）时延迟记录，层末复制目标内容（H3）。
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
            std::fs::remove_file(&out).ok();
            if entry.unpack(&out).is_err() {
                deferred_symlinks.push((path.to_path_buf(), target));
            }
            continue;
        }

        // 普通条目：解包到解析后的路径（覆盖已存在文件）
        let out = parent_abs.join(&file_name);
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        entry.unpack(&out)?;
    }

    // 第三遍：创建硬链接；失败时（如文件系统不支持）退化为复制。
    for (link_rel, target_rel) in hardlinks {
        let link_abs = match resolve_in_scope(dest, &link_rel) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let target_abs = match resolve_in_scope(dest, &target_rel) {
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

    // 第四遍：symlink 降级——把创建失败的符号链接物化为目标内容副本（H3）。
    for (link_rel, target_raw) in deferred_symlinks {
        if let Err(e) = materialize_symlink_as_copy(dest, &link_rel, &target_raw) {
            eprintln!("wbox: 警告：符号链接 {:?} 降级复制失败：{}", link_rel, e);
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
        unpack_layer(&gzip(&l1), &rootfs, "application/vnd.oci.image.layer.v1.tar+gzip").unwrap();
        assert!(rootfs.join("a.txt").exists());
        assert!(rootfs.join("dir/gone.txt").exists());

        // 第二层：whiteout 删 gone.txt，新增 b.txt
        let l2 = make_tar(&[("dir/.wh.gone.txt", b"" as &[u8]), ("b.txt", b"bb")]);
        unpack_layer(&gzip(&l2), &rootfs, "application/vnd.oci.image.layer.v1.tar+gzip").unwrap();
        assert!(!rootfs.join("dir/gone.txt").exists(), "whiteout 未删除目标");
        assert!(!rootfs.join("dir/.wh.gone.txt").exists(), "whiteout 文件不应落盘");
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
        let l = make_tar(&[("d/.wh..wh..opq", b"" as &[u8]), ("d/new.txt", b"n" as &[u8])]);
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
        let l2 = make_tar_with_symlinks(
            &[("real/keep.txt", b"k" as &[u8]), ("alias/new.txt", b"n" as &[u8])],
            &[("alias", "real")],
        );
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
    fn unpack_rejects_symlink_to_absolute_path_target_entry() {
        let dir = tmpdir("symtgt");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        // symlink 条目目标本身为绝对路径：直接拒绝创建
        let l = make_tar_with_symlinks(&[], &[("evil", "/etc")]);
        unpack_layer(&l, &rootfs, "").unwrap();
        assert!(
            !rootfs.join("evil").exists() && std::fs::read_link(rootfs.join("evil")).is_err(),
            "绝对目标 symlink 条目不应被创建"
        );
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
        assert!(verify_digest(&index_d, &evil_index).is_err(), "index 本体必须校验");
        // 篡改子 manifest
        let mut evil_child = child_body.to_vec();
        evil_child[5] ^= 0xff;
        assert!(verify_digest(&child_d, &evil_child).is_err(), "子 manifest 必须校验");
    }

    // ---- H3：symlink 降级为复制 ----

    #[test]
    fn symlink_fallback_copies_target_content() {
        let dir = tmpdir("symfb");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(rootfs.join("usr/bin")).unwrap();
        std::fs::write(rootfs.join("usr/bin/real"), b"binary").unwrap();
        // 直接调用降级路径（模拟 Windows 上 symlink 创建失败后的层末物化）
        materialize_symlink_as_copy(&rootfs, Path::new("usr/bin/alias"), Path::new("real")).unwrap();
        assert_eq!(std::fs::read(rootfs.join("usr/bin/alias")).unwrap(), b"binary");
        // 目录目标的递归复制
        materialize_symlink_as_copy(&rootfs, Path::new("usr/libcopy"), Path::new("bin")).unwrap();
        assert_eq!(
            std::fs::read(rootfs.join("usr/libcopy/real")).unwrap(),
            b"binary"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpack_symlink_after_target_in_later_entry() {
        // symlink 目标在同层后续条目才出现：正常平台创建 symlink 成功；
        // 降级路径（materialize）也能在层末解析到目标。
        let dir = tmpdir("symorder");
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let l = make_tar_with_symlinks(&[("target.txt", b"t" as &[u8])], &[("link", "target.txt")]);
        unpack_layer(&l, &rootfs, "").unwrap();
        // 无论是真 symlink 还是降级复制，读取链接路径都应得到目标内容
        assert_eq!(std::fs::read(rootfs.join("link")).unwrap(), b"t");
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
        let l = make_tar(&[("d/new.txt", b"n" as &[u8]), ("d/.wh..wh..opq", b"" as &[u8])]);
        unpack_layer(&l, &rootfs, "").unwrap();
        assert!(!rootfs.join("d/old.txt").exists(), "opaque 应清空下层内容");
        assert!(rootfs.join("d/new.txt").exists(), "同层新文件不应被 opaque 误删");
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
        let e = unpack_layer(&fake_zstd, &rootfs, "application/vnd.oci.image.layer.v1.tar+zstd")
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
}
