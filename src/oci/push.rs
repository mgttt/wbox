//! `wbox push`：把本地缓存里的镜像推回 registry（`PRD.md` F9.13）。
//!
//! # 推出去的是**平铺单层**，这一点必须先说清
//!
//! 本地缓存存的是**解包后的 rootfs**（`rootfs/` + 三个 json），pull 时层 tar
//! 解开就丢了——没有原始 blob 可以原样回推。所以 push 的做法是 flatten：把整个
//! `rootfs/` 重新打成**一个** `tar.gz` 层，配上单层 manifest 与 config 再推。
//!
//! 语义上等价于 `docker commit` 之后再 push：内容一致，**分层历史不保留**。
//! 对"改完再推自用"这类主要用途没有影响；但如果指望推上去还能与上游共享层，
//! 那是做不到的，命令会在开始时把这句话打出来，不让人误会。
//!
//! # 为什么不顺手做"保留分层"
//!
//! 那要求缓存改成保存原始压缩层 blob（外加解包结果），是存储布局的改动，
//! 牵动 pull/build/overlay 三条路径。它属于"镜像分层存储"那一格，与本格
//! 是两件事，PRD §2.4 已分开记。

use crate::error::{ErrKind, KindExt, Result, WboxError};
use anyhow::Context;
use std::io::Write;
use std::path::Path;

/// 单层 OCI 镜像用到的三个 media type。
const MT_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const MT_CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const MT_LAYER: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

/// 打包好的层：压缩字节 + 两个 digest。
///
/// `diff_id` 是**未压缩** tar 的 digest（config 的 `rootfs.diff_ids` 要它），
/// `digest` 是压缩后的（manifest 的 layer descriptor 要它）。两者不能混——
/// 混了 registry 校验能过，但拉回来时 diff_id 对不上。
#[derive(Debug)]
pub struct FlatLayer {
    // 压缩后的层可能几十 MB，不该出现在 Debug 输出里
    #[cfg_attr(test, allow(dead_code))]
    pub gzipped: Vec<u8>,
    pub digest: String,
    pub diff_id: String,
}

/// 计算 `sha256:<hex>`。供 build 侧写分层 manifest 时复用。
pub fn sha256_hex(data: &[u8]) -> String {
    sha256_of(data)
}

/// 计算 `sha256:<hex>`。
fn sha256_of(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    format!("sha256:{:x}", h.finalize())
}

/// 把 rootfs 目录打成单层 tar.gz。
///
/// 打包时**跳过 wbox 自己的产物**：`.wbox_oldroot` 是 pivot_root 的暂存目录，
/// 推上去只会让别人的镜像里多一个空目录。
pub fn flatten_rootfs(rootfs: &Path) -> Result<FlatLayer> {
    if !rootfs.is_dir() {
        return Err(WboxError::registry(format!(
            "rootfs 目录 '{}' 不存在（镜像是否已 pull？）",
            rootfs.display()
        )));
    }
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        // follow_symlinks(false)：rootfs 里的符号链接要原样进 tar。
        // 跟随的话会把链接展开成内容副本，既撑大体积又丢掉语义
        // （busybox 的 applet 链接就全废了）。
        builder.follow_symlinks(false);
        append_dir(&mut builder, rootfs, rootfs)?;
        builder
            .finish()
            .context("打包 rootfs 失败")
            .ctx(ErrKind::Registry)?;
    }
    let diff_id = sha256_of(&tar_bytes);

    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&tar_bytes)
        .and_then(|_| enc.try_finish())
        .context("压缩层失败")
        .ctx(ErrKind::Registry)?;
    let gzipped = enc.finish().context("压缩层失败").ctx(ErrKind::Registry)?;
    let digest = sha256_of(&gzipped);
    Ok(FlatLayer {
        gzipped,
        digest,
        diff_id,
    })
}

/// 把 `built` 相对 `base` 的**差异**打成一层 tar.gz（PRD L5b）。
///
/// 这是"构建产物 = 基础层 + 增量层"的关键：有了它，push 一个 build 出来的镜像
/// 时基础层会被 registry 的 `HEAD` 判定已存在而跳过上传，只传增量。
///
/// 三类差异，与 OCI 层规范一致：
/// - 新增/修改：原样入 tar（按内容比对，不看 mtime——构建每次都会刷新 mtime，
///   看 mtime 会把整棵树都判成"改过"，增量层就退化成全量）；
/// - 删除：写 `.wh.<name>` 空文件（`unpack_layer_with_state` 已认这个约定）；
/// - 未变：不入 tar。
pub fn diff_rootfs(base: &Path, built: &Path) -> Result<FlatLayer> {
    let mut tar_bytes = Vec::new();
    {
        let mut b = tar::Builder::new(&mut tar_bytes);
        b.follow_symlinks(false);
        diff_dir(&mut b, base, built, Path::new(""))?;
        b.finish().context("打包增量层失败").ctx(ErrKind::Registry)?;
    }
    let diff_id = sha256_of(&tar_bytes);
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&tar_bytes)
        .and_then(|_| enc.try_finish())
        .context("压缩增量层失败")
        .ctx(ErrKind::Registry)?;
    let gzipped = enc.finish().context("压缩增量层失败").ctx(ErrKind::Registry)?;
    let digest = sha256_of(&gzipped);
    Ok(FlatLayer {
        gzipped,
        digest,
        diff_id,
    })
}

/// 两棵树的同一相对目录做比对。
fn diff_dir<W: Write>(
    b: &mut tar::Builder<W>,
    base: &Path,
    built: &Path,
    rel: &Path,
) -> Result<()> {
    let fail = |what: &str, p: &Path, e: std::io::Error| {
        WboxError::registry(format!("{} '{}' 失败：{}", what, p.display(), e))
    };
    let built_dir = built.join(rel);
    let base_dir = base.join(rel);

    let mut built_names: Vec<std::ffi::OsString> = Vec::new();
    if built_dir.is_dir() {
        let rd = std::fs::read_dir(&built_dir).map_err(|e| fail("读取目录", &built_dir, e))?;
        for e in rd {
            built_names.push(e.map_err(|e| fail("枚举目录项", &built_dir, e))?.file_name());
        }
    }
    built_names.sort();

    for name in &built_names {
        let r = rel.join(name);
        // wbox 自己的产物不属于镜像内容
        if name == ".wbox_oldroot" {
            continue;
        }
        let bp = built.join(&r);
        let sp = base.join(&r);
        let meta = std::fs::symlink_metadata(&bp).map_err(|e| fail("读取元数据", &bp, e))?;
        if meta.is_dir() {
            // 目录本身在基础镜像里没有才需要入 tar；有的话只递归比内容
            if !sp.is_dir() {
                b.append_dir(&r, &bp)
                    .map_err(|e| fail("打包目录", &bp, e))?;
            }
            diff_dir(b, base, built, &r)?;
        } else if same_file(&sp, &bp) {
            continue;
        } else {
            b.append_path_with_name(&bp, &r)
                .map_err(|e| fail("打包", &bp, e))?;
        }
    }

    // 基础镜像里有、构建产物里没有的 → 删除，写 whiteout
    if base_dir.is_dir() {
        let rd = std::fs::read_dir(&base_dir).map_err(|e| fail("读取目录", &base_dir, e))?;
        for e in rd {
            let name = e.map_err(|e| fail("枚举目录项", &base_dir, e))?.file_name();
            if built_names.contains(&name) {
                continue;
            }
            let mut wh = std::ffi::OsString::from(".wh.");
            wh.push(&name);
            let mut hdr = tar::Header::new_gnu();
            hdr.set_size(0);
            hdr.set_mode(0o644);
            hdr.set_entry_type(tar::EntryType::Regular);
            hdr.set_cksum();
            b.append_data(&mut hdr, rel.join(&wh), std::io::empty())
                .map_err(|e| fail("写 whiteout", &base_dir.join(&name), e))?;
        }
    }
    Ok(())
}

/// 两个路径是否"内容相同"。**按内容比而不是 mtime**：构建过程会刷新 mtime，
/// 看 mtime 会把整棵树都判成改过，增量层就退化成全量。
fn same_file(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (
        std::fs::symlink_metadata(a),
        std::fs::symlink_metadata(b),
    ) else {
        return false;
    };
    if ma.file_type() != mb.file_type() {
        return false;
    }
    if ma.is_symlink() {
        return std::fs::read_link(a).ok() == std::fs::read_link(b).ok();
    }
    if ma.len() != mb.len() {
        return false;
    }
    std::fs::read(a).ok() == std::fs::read(b).ok()
}

/// 递归把目录内容加进 tar，路径相对 `base`。
fn append_dir<W: Write>(b: &mut tar::Builder<W>, base: &Path, dir: &Path) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("读取目录 '{}' 失败", dir.display()))
        .ctx(ErrKind::Registry)?;
    // 排序后再打包：同样的 rootfs 必须得到同样的 digest，否则"内容没变却
    // 每次都重传一层"，blob_exists 的省事也就白费了。read_dir 顺序不保证。
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    paths.sort();
    for path in paths {
        let rel = path
            .strip_prefix(base)
            .map_err(|_| WboxError::registry("打包时路径越出 rootfs"))?;
        // pivot_root 的暂存目录是 wbox 的产物，不属于镜像内容
        if rel.as_os_str() == ".wbox_oldroot" {
            continue;
        }
        let meta = std::fs::symlink_metadata(&path)
            .with_context(|| format!("读取 '{}' 元数据失败", path.display()))
            .ctx(ErrKind::Registry)?;
        if meta.is_dir() {
            b.append_dir(rel, &path)
                .with_context(|| format!("打包目录 '{}' 失败", path.display()))
                .ctx(ErrKind::Registry)?;
            append_dir(b, base, &path)?;
        } else {
            b.append_path_with_name(&path, rel)
                .with_context(|| format!("打包 '{}' 失败", path.display()))
                .ctx(ErrKind::Registry)?;
        }
    }
    Ok(())
}

/// 由本地 `config.json` 与新层的 diff_id 生成推送用的 image config。
///
/// 尽量沿用本地已有的 `config` 段（Env/Cmd/Entrypoint/WorkingDir 都在里面），
/// 只把 `rootfs.diff_ids` 换成平铺后的单个 id、`history` 清空——保留旧 history
/// 会与"只有一层"自相矛盾，拉取方按 history 复原会对不上。
pub fn build_config(local_config: &[u8], diff_id: &str, arch: &str, os: &str) -> Result<Vec<u8>> {
    let mut v: serde_json::Value = serde_json::from_slice(local_config)
        .context("本地 config.json 不是合法 JSON")
        .ctx(ErrKind::Registry)?;
    if !v.is_object() {
        v = serde_json::json!({});
    }
    let obj = v.as_object_mut().expect("已确保是 object");
    obj.insert("architecture".into(), serde_json::json!(arch));
    obj.insert("os".into(), serde_json::json!(os));
    obj.insert(
        "rootfs".into(),
        serde_json::json!({ "type": "layers", "diff_ids": [diff_id] }),
    );
    obj.insert("history".into(), serde_json::json!([]));
    obj.entry("config").or_insert_with(|| serde_json::json!({}));
    serde_json::to_vec(&v)
        .context("序列化 config 失败")
        .ctx(ErrKind::Registry)
}

/// 生成单层 manifest。
pub fn build_manifest(
    config_digest: &str,
    config_size: usize,
    layer_digest: &str,
    layer_size: usize,
) -> Result<Vec<u8>> {
    let m = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": MT_MANIFEST,
        "config": {
            "mediaType": MT_CONFIG,
            "digest": config_digest,
            "size": config_size,
        },
        "layers": [{
            "mediaType": MT_LAYER,
            "digest": layer_digest,
            "size": layer_size,
        }],
    });
    serde_json::to_vec(&m)
        .context("序列化 manifest 失败")
        .ctx(ErrKind::Registry)
}

/// 原样回推所需的全部材料：原始 manifest 字节 + 它引用的每个 blob。
struct Verbatim {
    manifest: Vec<u8>,
    media_type: String,
    /// (digest, 字节)，顺序为 layers 在前、config 在后无所谓——都要先于 manifest 推。
    blobs: Vec<(String, Vec<u8>)>,
}

/// 判断能否**原样回推**，能就把材料备齐。
///
/// 条件很硬：本地存的 manifest 必须是真的 manifest（不是 build 写的 `{}` 占位），
/// 且它引用的每个 blob 都还在 `blobs/` 里。任一条不满足就退回 flatten——
/// 退回时会打印原因，不静默改变语义。
///
/// 满足时 push 的 manifest 字节与拉下来时**一字不差**，故 digest 不变，
/// 层也与上游共享。这正是 L5 的判据。
fn try_verbatim(dir: &std::path::Path) -> Option<Verbatim> {
    let manifest = std::fs::read(dir.join("manifest.json")).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&manifest).ok()?;
    let config_digest = v.get("config")?.get("digest")?.as_str()?.to_string();
    let layers = v.get("layers")?.as_array()?;
    if layers.is_empty() {
        return None;
    }
    let media_type = v
        .get("mediaType")
        .and_then(|m| m.as_str())
        .unwrap_or(MT_MANIFEST)
        .to_string();

    let mut blobs = Vec::new();
    for l in layers {
        let d = l.get("digest")?.as_str()?;
        let bytes = std::fs::read(super::blob_path(dir, d)).ok()?;
        blobs.push((d.to_string(), bytes));
    }
    // config 的原始字节就是 config.json（pull 时按 blob 原样写下）。
    // 校验它的 digest 与 manifest 里写的一致——不一致说明缓存被动过，
    // 那时原样回推会推出一个自相矛盾的镜像。
    let config = std::fs::read(dir.join("config.json")).ok()?;
    if sha256_of(&config) != config_digest {
        return None;
    }
    blobs.push((config_digest, config));
    Some(Verbatim {
        manifest,
        media_type,
        blobs,
    })
}

/// `wbox push` 的入口编排。
pub fn push(image_ref: &str, registry_override: Option<&str>, verbose: bool) -> Result<()> {
    let iref = super::ImageRef::parse(image_ref, registry_override)?;
    let dir = super::image_dir(&iref)?;
    if !dir.is_dir() {
        return Err(WboxError::registry(format!(
            "本地没有镜像 '{}'（先 pull 或 build）",
            iref.qualified_ref()
        )));
    }
    let client = super::registry::RegistryClient::new(&iref.registry);

    // 优先原样回推：本地留着原始压缩层时，推上去的 manifest 与拉下来的一字
    // 不差，digest 不变、层与上游共享（PRD L5）。
    if let Some(v) = try_verbatim(&dir) {
        println!(
            "wbox: 推送 {} —— **原样回推**（保留原始分层，manifest digest 不变）",
            iref.qualified_ref()
        );
        // 顺序不能反：manifest 引用的 blob 必须先在 registry 上存在。
        let mut uploaded = 0usize;
        let mut skipped = 0usize;
        for (digest, bytes) in &v.blobs {
            let sent = client.push_blob(&iref.repo, digest, bytes)?;
            if sent {
                uploaded += 1;
            } else {
                skipped += 1;
            }
            if verbose {
                println!(
                    "wbox: {} {}（{} 字节）",
                    if sent { "上传" } else { "跳过（registry 已有）" },
                    digest,
                    bytes.len()
                );
            }
        }
        if skipped > 0 {
            // 这句是分层复用的收益体现，值得默认打印
            println!("wbox: {} 层已在 registry 上，跳过上传；实传 {} 层", skipped, uploaded);
        }
        let reported =
            client.put_manifest(&iref.repo, &iref.reference, &v.media_type, &v.manifest)?;
        let local = sha256_of(&v.manifest);
        println!("wbox: 完成 —— {}", iref.qualified_ref());
        println!("wbox: manifest digest: {}", reported.clone().unwrap_or_else(|| local.clone()));
        // registry 回报的 digest 若与本地算的不同，说明它改写了 manifest，
        // "原样"这个承诺就没兑现——必须说出来，不能让用户以为分层还在。
        if let Some(r) = reported {
            if r != local {
                eprintln!(
                    "wbox: 警告: registry 回报的 digest（{}）与本地 manifest 的（{}）不同，\
                     说明它改写了 manifest，原样回推未完全兑现",
                    r, local
                );
            }
        }
        return Ok(());
    }

    // 退回 flatten。**打印原因**：推上去的与本地"看起来一样"，但分层历史没了，
    // 不说的话用户会以为原样回推成功了。
    println!(
        "wbox: 推送 {} —— 本地没有原始压缩层（旧缓存或 build 产物），\
         rootfs 将平铺为**单层**，语义等价 docker commit 后 push",
        iref.qualified_ref()
    );

    let layer = flatten_rootfs(&dir.join("rootfs"))?;
    if verbose {
        println!(
            "wbox: 层 digest={} 压缩后 {} 字节",
            layer.digest,
            layer.gzipped.len()
        );
    }

    let local_config = std::fs::read(dir.join("config.json")).unwrap_or_else(|_| b"{}".to_vec());
    let config_bytes = build_config(&local_config, &layer.diff_id, "amd64", "linux")?;
    let config_digest = sha256_of(&config_bytes);
    let manifest_bytes = build_manifest(
        &config_digest,
        config_bytes.len(),
        &layer.digest,
        layer.gzipped.len(),
    )?;

    // 顺序不能反：manifest 引用的 blob 必须**先**在 registry 上存在，
    // 否则符合规范的 registry 会以 MANIFEST_BLOB_UNKNOWN 拒绝。
    client.push_blob(&iref.repo, &layer.digest, &layer.gzipped)?;
    client.push_blob(&iref.repo, &config_digest, &config_bytes)?;
    let digest = client.put_manifest(&iref.repo, &iref.reference, MT_MANIFEST, &manifest_bytes)?;

    println!("wbox: 完成 —— {}", iref.qualified_ref());
    println!(
        "wbox: manifest digest: {}",
        digest.unwrap_or_else(|| sha256_of(&manifest_bytes))
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wbox-push-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 同样的 rootfs 必须得到同样的 digest。read_dir 顺序不保证，不排序的话
    /// 内容没变也会每次算出新 digest，blob_exists 的跳过就永远命不中。
    #[test]
    fn flatten_is_deterministic() {
        let d = temp("det");
        std::fs::create_dir_all(d.join("rootfs/bin")).unwrap();
        for n in ["a", "b", "c", "d", "e"] {
            std::fs::write(d.join("rootfs/bin").join(n), n.as_bytes()).unwrap();
        }
        let one = flatten_rootfs(&d.join("rootfs")).unwrap();
        let two = flatten_rootfs(&d.join("rootfs")).unwrap();
        assert_eq!(one.digest, two.digest, "同一 rootfs 两次打包 digest 应相同");
        assert_eq!(one.diff_id, two.diff_id);
        assert_ne!(one.digest, one.diff_id, "压缩前后的 digest 不该相同");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 内容变了 digest 必须跟着变——否则改完推上去的还是旧的。
    #[test]
    fn flatten_tracks_content_changes() {
        let d = temp("chg");
        std::fs::create_dir_all(d.join("rootfs")).unwrap();
        std::fs::write(d.join("rootfs/f"), b"one").unwrap();
        let before = flatten_rootfs(&d.join("rootfs")).unwrap().digest;
        std::fs::write(d.join("rootfs/f"), b"two").unwrap();
        let after = flatten_rootfs(&d.join("rootfs")).unwrap().digest;
        assert_ne!(before, after);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// `.wbox_oldroot` 是 pivot_root 的暂存目录，是 wbox 的产物不是镜像内容。
    #[test]
    fn flatten_skips_wbox_artifacts() {
        let d = temp("skip");
        std::fs::create_dir_all(d.join("rootfs/.wbox_oldroot")).unwrap();
        std::fs::write(d.join("rootfs/real"), b"x").unwrap();
        let with_artifact = flatten_rootfs(&d.join("rootfs")).unwrap().diff_id;
        std::fs::remove_dir_all(d.join("rootfs/.wbox_oldroot")).unwrap();
        let without = flatten_rootfs(&d.join("rootfs")).unwrap().diff_id;
        assert_eq!(with_artifact, without, "暂存目录不该影响层内容");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 原样回推的条件很硬，任一条不满足都要退回 flatten——而不是推出一个
    /// 自相矛盾的镜像。
    #[test]
    fn verbatim_requires_manifest_and_every_blob() {
        let d = temp("verb");
        let config = br#"{"architecture":"amd64"}"#.to_vec();
        let cdig = sha256_of(&config);
        let l1 = b"layer-one-bytes".to_vec();
        let l2 = b"layer-two-bytes".to_vec();
        let (d1, d2) = (sha256_of(&l1), sha256_of(&l2));
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": MT_MANIFEST,
            "config": {"mediaType": MT_CONFIG, "digest": cdig, "size": config.len()},
            "layers": [
                {"mediaType": MT_LAYER, "digest": d1, "size": l1.len()},
                {"mediaType": MT_LAYER, "digest": d2, "size": l2.len()},
            ],
        });
        let mbytes = serde_json::to_vec(&manifest).unwrap();
        std::fs::write(d.join("manifest.json"), &mbytes).unwrap();
        std::fs::write(d.join("config.json"), &config).unwrap();
        std::fs::create_dir_all(d.join(super::super::BLOBS_DIR)).unwrap();
        std::fs::write(super::super::blob_path(&d, &d1), &l1).unwrap();

        // 只有一个层 blob 在：不够，必须退回
        assert!(try_verbatim(&d).is_none(), "缺层 blob 时不该原样回推");

        std::fs::write(super::super::blob_path(&d, &d2), &l2).unwrap();
        let v = try_verbatim(&d).expect("材料齐了应可原样回推");
        // manifest 字节必须**一字不差**——digest 不变正是靠这个
        assert_eq!(v.manifest, mbytes);
        assert_eq!(v.media_type, MT_MANIFEST);
        // 两个层 + 一个 config
        assert_eq!(v.blobs.len(), 3);
        assert!(v.blobs.iter().any(|(dg, _)| dg == &d1));
        assert!(v.blobs.iter().any(|(dg, _)| dg == &cdig));

        // config 被动过：digest 对不上，必须退回而不是推个自相矛盾的镜像
        std::fs::write(d.join("config.json"), br#"{"architecture":"arm64"}"#).unwrap();
        assert!(try_verbatim(&d).is_none(), "config digest 不符时不该原样回推");

        let _ = std::fs::remove_dir_all(&d);
    }

    /// build 产物的 manifest.json 是 `{}` 占位，没有层可推——必须退回 flatten。
    #[test]
    fn placeholder_manifest_falls_back_to_flatten() {
        let d = temp("plc");
        std::fs::write(d.join("manifest.json"), b"{}").unwrap();
        std::fs::write(d.join("config.json"), b"{}").unwrap();
        assert!(try_verbatim(&d).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_rootfs_is_an_error() {
        let d = temp("none");
        let e = flatten_rootfs(&d.join("nope")).unwrap_err();
        assert!(format!("{}", e).contains("pull"), "要提示怎么办：{}", e);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// config 要沿用本地的 Env/Cmd，但 diff_ids 换成平铺后的单个、history 清空
    /// ——留着旧 history 会与"只有一层"自相矛盾。
    #[test]
    fn config_keeps_runtime_fields_but_resets_layers() {
        let local = br#"{"config":{"Env":["A=1"],"Cmd":["/bin/sh"]},
            "rootfs":{"type":"layers","diff_ids":["sha256:old1","sha256:old2"]},
            "history":[{"created_by":"old"}]}"#;
        let out = build_config(local, "sha256:new", "amd64", "linux").unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["config"]["Env"][0], "A=1");
        assert_eq!(v["config"]["Cmd"][0], "/bin/sh");
        assert_eq!(v["rootfs"]["diff_ids"].as_array().unwrap().len(), 1);
        assert_eq!(v["rootfs"]["diff_ids"][0], "sha256:new");
        assert_eq!(v["history"].as_array().unwrap().len(), 0);
        assert_eq!(v["architecture"], "amd64");
        assert_eq!(v["os"], "linux");
    }

    #[test]
    fn config_tolerates_garbage_local_file() {
        assert!(build_config(b"[]", "sha256:x", "amd64", "linux").is_ok());
        assert!(build_config(b"not json", "sha256:x", "amd64", "linux").is_err());
    }

    #[test]
    fn manifest_shape_matches_oci_single_layer() {
        let m = build_manifest("sha256:c", 12, "sha256:l", 34).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&m).unwrap();
        assert_eq!(v["schemaVersion"], 2);
        assert_eq!(v["mediaType"], MT_MANIFEST);
        assert_eq!(v["config"]["digest"], "sha256:c");
        assert_eq!(v["config"]["size"], 12);
        assert_eq!(v["layers"].as_array().unwrap().len(), 1);
        assert_eq!(v["layers"][0]["mediaType"], MT_LAYER);
        assert_eq!(v["layers"][0]["digest"], "sha256:l");
        assert_eq!(v["layers"][0]["size"], 34);
    }
}
