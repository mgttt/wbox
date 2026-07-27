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
    // 这句必须打出来：推上去的与本地"看起来一样"，但分层历史没了。
    // 不说的话，用户会以为推回去的还是原来那个分层镜像。
    println!(
        "wbox: 推送 {} —— rootfs 将平铺为**单层**（本地缓存不保留原始层，\
         语义等价 docker commit 后 push）",
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

    let client = super::registry::RegistryClient::new(&iref.registry);
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
