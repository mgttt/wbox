//! `wbox save` / `wbox load`：把镜像打包成 tar 搬到别处（`PRD.md` F9.22）。
//!
//! # 为什么值得做
//!
//! 本产品的目标用户里有"受管 Windows 机器上的开发者"（§3.1），那种环境常常
//! 连不上 registry，或者出网要走审批。`save`/`load` 让镜像能用一个文件搬过去，
//! 不必架设 registry——这正是 docker `save`/`load` 存在的理由。
//!
//! # 打的是**整个缓存目录**，不是 rootfs
//!
//! 归档里含 `rootfs/`、三个 json，以及 F9.16 留下的 `blobs/`（原始压缩层）。
//! 带上 blobs 才能让搬过去的镜像仍然可以**原样 push**（digest 不变）；
//! 只打 rootfs 的话，load 出来的镜像会退化成"只能 flatten 推送"。
//!
//! # load 的安全约束
//!
//! tar 是外部输入，路径穿越是这里唯一真正危险的东西。除了逐条拒绝绝对路径与
//! `..`，还**按白名单**限定顶层条目（只收本模块自己写出去的那几个名字）——
//! 与其事后判断"这个路径安全吗"，不如一开始就只认识我们自己产的结构。

use crate::error::{ErrKind, KindExt, Result, WboxError};
use crate::fault::Context;
use std::path::{Component, Path, PathBuf};

/// 归档里记录镜像身份的文件。load 靠它知道该放回哪个缓存位置。
const MARKER: &str = "wbox-image.json";

/// 允许出现在归档顶层的条目。白名单而非黑名单：只认识我们自己产的结构。
const ALLOWED_TOP: &[&str] = &[
    "rootfs",
    "blobs",
    "manifest.json",
    "config.json",
    "layers.json",
    MARKER,
];

/// `wbox save`：把镜像缓存目录打成 tar。
pub fn save(image_ref: &str, out: &Path, registry_override: Option<&str>) -> Result<()> {
    let iref = super::ImageRef::parse(image_ref, registry_override)?;
    let dir = super::image_dir(&iref)?;
    if !dir.join("rootfs").is_dir() {
        return Err(WboxError::registry(format!(
            "本地没有镜像 '{}'（先 pull 或 build）",
            iref.qualified_ref()
        )));
    }
    let file = std::fs::File::create(out)
        .with_context(|| format!("创建 '{}' 失败", out.display()))
        .ctx(ErrKind::Registry)?;
    let mut b = wbox_codec::tar::Builder::new(file);
    b.follow_symlinks(false);
    // 身份先写进去：load 端不必靠文件名或调用方记忆去猜这是哪个镜像
    let marker = wbox_codec::json!({ "ref": iref.qualified_ref() }).to_string();
    let mut hdr = wbox_codec::tar::Header::new_gnu();
    hdr.set_size(marker.len() as u64);
    hdr.set_mode(0o644);
    hdr.set_entry_type(wbox_codec::tar::EntryType::Regular);
    hdr.set_cksum();
    b.append_data(&mut hdr, MARKER, marker.as_bytes())
        .context("写归档标记失败")
        .ctx(ErrKind::Registry)?;
    super::push::append_dir(&mut b, &dir, &dir)?;
    b.finish().context("打包镜像失败").ctx(ErrKind::Registry)?;
    println!("wbox: 已保存 {} → {}", iref.qualified_ref(), out.display());
    Ok(())
}

/// `wbox load`：把 tar 还原成本地镜像。`tag_override` 非 None 时改名落地。
pub fn load(archive: &Path, tag_override: Option<&str>) -> Result<()> {
    let bytes = std::fs::read(archive)
        .with_context(|| format!("读取 '{}' 失败", archive.display()))
        .ctx(ErrKind::Registry)?;
    // 先扫一遍取身份：要先知道落到哪，才好在同一个卷上原子替换
    let stored_ref = read_marker(&bytes)?;
    let want = tag_override.unwrap_or(&stored_ref);
    let iref = super::ImageRef::parse(want, None)?;
    let dest = super::image_dir(&iref)?;

    let staging = dest.with_extension("wbox-load");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .context("创建解包暂存目录失败")
        .ctx(ErrKind::Registry)?;
    let mut ar = wbox_codec::tar::Archive::new(std::io::Cursor::new(&bytes));
    for entry in ar
        .entries()
        .context("读取归档失败")
        .ctx(ErrKind::Registry)?
    {
        let e = entry.context("读取归档条目失败").ctx(ErrKind::Registry)?;
        let path = e
            .path()
            .context("归档条目路径非法")
            .ctx(ErrKind::Registry)?
            .into_owned();
        let rel = safe_relative(&path)?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = staging.join(&rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .context("创建解包目录失败")
                .ctx(ErrKind::Registry)?;
        }
        e.unpack(&target)
            .with_context(|| format!("解包 '{}' 失败", rel.display()))
            .ctx(ErrKind::Registry)?;
    }
    let _ = std::fs::remove_file(staging.join(MARKER));
    if !staging.join("rootfs").is_dir() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(WboxError::registry(
            "归档里没有 rootfs，不是 wbox save 产出的镜像包",
        ));
    }
    let _ = std::fs::remove_dir_all(&dest);
    if let Some(p) = dest.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    std::fs::rename(&staging, &dest)
        .context("落地镜像失败")
        .ctx(ErrKind::Registry)?;
    println!("wbox: 已载入 {} ← {}", iref.qualified_ref(), archive.display());
    Ok(())
}

fn read_marker(bytes: &[u8]) -> Result<String> {
    let mut ar = wbox_codec::tar::Archive::new(std::io::Cursor::new(bytes));
    for entry in ar.entries().context("读取归档失败").ctx(ErrKind::Registry)? {
        let mut e = entry.context("读取归档条目失败").ctx(ErrKind::Registry)?;
        let is_marker = e
            .path()
            .map(|p| p.as_os_str() == MARKER)
            .unwrap_or(false);
        if is_marker {
            let mut s = String::new();
            use std::io::Read;
            e.read_to_string(&mut s)
                .context("读取归档标记失败")
                .ctx(ErrKind::Registry)?;
            let v: wbox_codec::json::Value = wbox_codec::json::from_str(&s)
                .context("归档标记不是合法 JSON")
                .ctx(ErrKind::Registry)?;
            if let Some(r) = v.get("ref").and_then(|r| r.as_str()) {
                return Ok(r.to_string());
            }
        }
    }
    Err(WboxError::registry(
        "归档里没有 wbox-image.json：不是 wbox save 产出的镜像包（可用 -t 指定要载入成哪个镜像）",
    ))
}

/// 校验并归一化归档里的相对路径。
///
/// 三道闸门：不能是绝对路径、不能含 `..`、顶层必须在白名单里。
/// tar 是外部输入，这里放松一点就是任意文件写。
fn safe_relative(p: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for (i, c) in p.components().enumerate() {
        match c {
            Component::Normal(seg) => {
                if i == 0 && !ALLOWED_TOP.iter().any(|a| *a == seg.to_string_lossy()) {
                    return Err(WboxError::registry(format!(
                        "归档含预期之外的顶层条目 '{}'，拒绝解包",
                        seg.to_string_lossy()
                    )));
                }
                out.push(seg);
            }
            Component::CurDir => {}
            _ => {
                return Err(WboxError::registry(format!(
                    "归档条目路径 '{}' 含绝对路径或 '..'，拒绝解包",
                    p.display()
                )))
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tar 是外部输入，路径穿越是这里唯一真正危险的东西。
    #[test]
    fn rejects_traversal_and_unexpected_top_level() {
        assert!(safe_relative(Path::new("rootfs/bin/sh")).is_ok());
        assert!(safe_relative(Path::new("blobs/sha256_x")).is_ok());
        assert!(safe_relative(Path::new("manifest.json")).is_ok());
        // 绝对路径 / 上跳 / 不认识的顶层，一律拒绝
        assert!(safe_relative(Path::new("/etc/passwd")).is_err());
        assert!(safe_relative(Path::new("../evil")).is_err());
        assert!(safe_relative(Path::new("rootfs/../../evil")).is_err());
        assert!(safe_relative(Path::new("evil/x")).is_err());
    }

    #[test]
    fn marker_missing_is_reported_with_the_escape_hatch() {
        let mut buf = Vec::new();
        {
            let mut b = wbox_codec::tar::Builder::new(&mut buf);
            b.finish().unwrap();
        }
        let m = format!("{}", read_marker(&buf).unwrap_err());
        assert!(m.contains("wbox save"), "{}", m);
        assert!(m.contains("-t"), "要指出逃生口：{}", m);
    }
}
