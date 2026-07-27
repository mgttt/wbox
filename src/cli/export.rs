//! `wbox export` / `wbox import`：容器文件系统的裸 tar 搬运（`PRD.md` F9.25）。
//!
//! # 与 `save`/`load`（F9.22）是两件事
//!
//! 这一点 docker 的用户也常搞混，所以两边的错误信息都写明白：
//!
//! | | 搬的是什么 | 带不带历史/配置 |
//! |---|---|---|
//! | `save` / `load` | **镜像** | 带：manifest、config、原始压缩层，load 回来还能原样 push |
//! | `export` / `import` | **容器的当前文件系统** | 不带：就是一棵 rootfs 压成 tar，层历史被压平 |
//!
//! `export` 的典型用途是"把这个容器现在的样子交出去"——交给不装 wbox 的人、
//! 或者塞进别的构建流程。要的正是"没有 wbox 特有结构"的裸 tar。
//!
//! # 实现上几乎没有新东西
//!
//! 容器的完整文件系统 = 镜像下层 + overlay upper 合并，而这件事
//! [`crate::layers::ContainerLayers::materialize`] 已经做了（`commit` 用的是
//! 同一个）。所以 `export` = 物化到临时目录 + 打 tar。
//!
//! `import` 反过来：解一棵 rootfs 出来，补上最小的 config/manifest，落成镜像。
//! **解包的安全约束比 `load` 更强**：`load` 收的是 wbox 自己产的结构，可以按
//! 白名单认顶层；`import` 收的是任意来源的 rootfs tar，顶层无从白名单化，
//! 所以只能逐条挡绝对路径与 `..`，并且**全部落在 `rootfs/` 之下**——
//! 归档里的路径永远拼不出 `rootfs/` 以外的位置。

use crate::error::{ErrKind, KindExt, Result, WboxError};
use anyhow::Context;
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
struct ExportOpts {
    container: String,
    out: PathBuf,
}

fn parse_export(args: &[String]) -> Result<ExportOpts> {
    let mut out: Option<String> = None;
    let mut container: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => out = Some(super::args::take_value(args, &mut i, "--output")?),
            other if other.starts_with('-') => {
                return Err(WboxError::args(format!(
                    "export: 未知参数 '{}'（用法：wbox export -o <FILE> <NAME>）",
                    other
                )))
            }
            other => {
                if container.is_some() {
                    return Err(WboxError::args("export: 一次只能导出一个容器"));
                }
                container = Some(other.to_string());
            }
        }
        i += 1;
    }
    let container = container
        .ok_or_else(|| WboxError::args("export: 缺少容器名（用法：wbox export -o <FILE> <NAME>）"))?;
    // 与 `save` 同一条理由：不默认写 stdout。容器 rootfs 动辄几十 MB，
    // 误写进终端是灾难性的体验，而那个默认在管道场景之外几乎只会伤人。
    let out = out.ok_or_else(|| {
        WboxError::args("export 需要 -o <FILE>：不默认写 stdout（rootfs 动辄几十 MB，误写进终端是灾难）")
    })?;
    Ok(ExportOpts {
        container,
        out: PathBuf::from(out),
    })
}

pub fn cmd_export(args: &[String]) -> Result<u32> {
    let o = parse_export(args)?;
    #[cfg(windows)]
    {
        Err(WboxError::args(format!(
            "export（{} → {}）目前只在 Linux 宿主可用：它靠 overlay 可写层取得容器的\
             文件系统，Windows 侧尚未提供该层（PRD F9.12/F9.25）",
            o.container,
            o.out.display()
        )))
    }
    #[cfg(not(windows))]
    {
        let layers = crate::layers::ContainerLayers::resolve(&o.container, "无法 export")?;
        // 物化到临时目录再打包。直接边遍历分层边写 tar 也行，但那要在打包侧
        // 重新实现一遍 whiteout/opaque 的合并规则——两份规则迟早会不一致。
        let staging = crate::runstate::resolve_existing(&o.container)?.join("export-staging");
        let _ = std::fs::remove_dir_all(&staging);
        layers.materialize(&staging)?;

        let file = std::fs::File::create(&o.out)
            .with_context(|| format!("创建 '{}' 失败", o.out.display()))
            .ctx(ErrKind::Args)?;
        let mut b = tar::Builder::new(file);
        b.follow_symlinks(false);
        let r = crate::oci::push::append_dir(&mut b, &staging, &staging)
            .and_then(|_| b.finish().context("打包失败").ctx(ErrKind::Args));
        // 无论成败都收拾暂存目录：留下来会让容器状态目录里多出一棵完整 rootfs，
        // 悄悄吃掉一份磁盘。
        let _ = std::fs::remove_dir_all(&staging);
        r?;
        println!(
            "wbox: 已导出容器 {} 的文件系统 → {}",
            o.container,
            o.out.display()
        );
        Ok(0)
    }
}

#[derive(Debug)]
struct ImportOpts {
    tag: String,
    file: PathBuf,
}

fn parse_import(args: &[String]) -> Result<ImportOpts> {
    let mut tag: Option<String> = None;
    let mut file: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-t" | "--tag" => tag = Some(super::args::take_value(args, &mut i, "--tag")?),
            other if other.starts_with('-') => {
                return Err(WboxError::args(format!(
                    "import: 未知参数 '{}'（用法：wbox import -t <IMAGE[:TAG]> <FILE>）",
                    other
                )))
            }
            other => {
                if file.is_some() {
                    return Err(WboxError::args("import: 一次只能载入一个归档"));
                }
                file = Some(other.to_string());
            }
        }
        i += 1;
    }
    let file = file
        .ok_or_else(|| WboxError::args("import: 缺少归档路径（用法：wbox import -t <IMAGE> <FILE>）"))?;
    // 裸 rootfs tar 里**没有任何身份信息**（这正是它与 wbox save 归档的区别），
    // 所以 -t 是必需的，猜不出来。
    let tag = tag.ok_or_else(|| {
        WboxError::args(
            "import 需要 -t <IMAGE[:TAG]>：裸 rootfs 归档里不含镜像身份，无从推断\
             （带身份的归档请用 wbox load）",
        )
    })?;
    Ok(ImportOpts {
        tag,
        file: PathBuf::from(file),
    })
}

pub fn cmd_import(args: &[String]) -> Result<u32> {
    let o = parse_import(args)?;
    let iref = crate::oci::ImageRef::parse(&o.tag, None)?;
    let dest = crate::oci::image_dir(&iref)?;
    let bytes = std::fs::read(&o.file)
        .with_context(|| format!("读取 '{}' 失败", o.file.display()))
        .ctx(ErrKind::Registry)?;

    let staging = dest.with_extension("wbox-import");
    let _ = std::fs::remove_dir_all(&staging);
    let rootfs = staging.join("rootfs");
    std::fs::create_dir_all(&rootfs)
        .context("创建解包暂存目录失败")
        .ctx(ErrKind::Registry)?;

    let mut count = 0usize;
    let mut ar = tar::Archive::new(std::io::Cursor::new(&bytes));
    for entry in ar.entries().context("读取归档失败").ctx(ErrKind::Registry)? {
        let mut e = entry.context("读取归档条目失败").ctx(ErrKind::Registry)?;
        let path = e
            .path()
            .context("归档条目路径非法")
            .ctx(ErrKind::Registry)?
            .into_owned();
        let rel = match safe_rootfs_relative(&path) {
            Ok(r) => r,
            Err(err) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(err);
            }
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = rootfs.join(&rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .context("创建解包目录失败")
                .ctx(ErrKind::Registry)?;
        }
        // 解包失败不中止整个归档：裸 rootfs 里常有本机建不出来的条目
        // （设备节点要 root，属主可能不存在）。中止的话，绝大多数 rootfs
        // 都 import 不进来——那才是真正无用的行为。
        if e.unpack(&target).is_ok() {
            count += 1;
        }
    }
    if count == 0 {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(WboxError::registry(format!(
            "'{}' 里没有可用条目：不像是一棵 rootfs 的 tar（带 wbox 身份的镜像归档请用 wbox load）",
            o.file.display()
        )));
    }

    // 补最小可用元数据。**如实写空的 entrypoint/cmd**：裸 rootfs 里确实没有这些
    // 信息，编一个 `/bin/sh` 进去会让 `wbox run <镜像>` 表现得像是镜像自带的默认
    // 命令，而它其实是 wbox 猜的。
    std::fs::write(
        staging.join("config.json"),
        serde_json::json!({
            "config": { "Env": [], "Cmd": [], "Entrypoint": [], "WorkingDir": "" }
        })
        .to_string(),
    )
    .context("写 config.json 失败")
    .ctx(ErrKind::Registry)?;
    // 没有原始层，push 时只能 flatten 成单层（与 build 产物同一条路，F9.13）
    std::fs::write(staging.join("manifest.json"), "{}")
        .context("写 manifest.json 失败")
        .ctx(ErrKind::Registry)?;
    std::fs::write(staging.join("layers.json"), "[]")
        .context("写 layers.json 失败")
        .ctx(ErrKind::Registry)?;

    let _ = std::fs::remove_dir_all(&dest);
    if let Some(p) = dest.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    std::fs::rename(&staging, &dest)
        .context("落地镜像失败")
        .ctx(ErrKind::Registry)?;
    println!(
        "wbox: 已从 {} 导入镜像 {}（{} 个条目；无层历史，push 时会压平成单层）",
        o.file.display(),
        iref.qualified_ref(),
        count
    );
    Ok(0)
}

/// 校验归档里的路径，并把它**限死在 `rootfs/` 之下**。
///
/// 与 `load` 的白名单不同：`import` 收的是任意来源的 rootfs tar，顶层是什么
/// （`bin`/`etc`/`usr`/……）由归档决定，无从白名单化。能守住的只有两条——
/// 不许绝对路径、不许 `..`——加上调用方一律拼到 `rootfs/` 下。两条合起来，
/// 归档里的任何路径都拼不出 `rootfs/` 以外的位置。
fn safe_rootfs_relative(p: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(seg) => out.push(seg),
            Component::CurDir => {}
            // RootDir / ParentDir / Prefix 全部拒绝。**不做"剥掉前导 / 再继续"**
            // 这种宽容处理：宽容意味着要论证剥完之后仍然安全，而拒绝不需要论证。
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

    #[test]
    fn export_requires_output_and_one_container() {
        let a: Vec<String> = ["-o", "/tmp/x.tar", "c"].iter().map(|s| s.to_string()).collect();
        let o = parse_export(&a).unwrap();
        assert_eq!(o.container, "c");
        assert_eq!(o.out, PathBuf::from("/tmp/x.tar"));
        // 不默认写 stdout，理由要说出来
        let m = format!("{}", parse_export(&["c".to_string()]).unwrap_err());
        assert!(m.contains("stdout"), "{}", m);
        assert!(parse_export(&[]).is_err());
        assert!(parse_export(&["-o".to_string(), "f".to_string(), "a".to_string(), "b".to_string()]).is_err());
        assert!(parse_export(&["--bogus".to_string()]).is_err());
    }

    /// 裸 rootfs 里没有镜像身份，`-t` 是必需的；错误还要指出带身份的归档该用
    /// `load`——这两条命令最容易被搞混。
    #[test]
    fn import_requires_tag_and_points_at_load() {
        let a: Vec<String> = ["-t", "mine:v1", "/tmp/x.tar"].iter().map(|s| s.to_string()).collect();
        let o = parse_import(&a).unwrap();
        assert_eq!(o.tag, "mine:v1");
        let m = format!("{}", parse_import(&["/tmp/x.tar".to_string()]).unwrap_err());
        assert!(m.contains("wbox load"), "要指出该用哪条命令：{}", m);
        assert!(parse_import(&["-t".to_string(), "x".to_string()]).is_err(), "缺文件应报错");
    }

    /// 任意来源的 tar 是外部输入，路径穿越是这里唯一真正危险的东西。
    /// 顶层无从白名单化（rootfs 的顶层由归档决定），能守住的就这两条。
    #[test]
    fn rejects_absolute_and_traversal_paths() {
        assert_eq!(
            safe_rootfs_relative(Path::new("bin/sh")).unwrap(),
            PathBuf::from("bin/sh")
        );
        assert_eq!(
            safe_rootfs_relative(Path::new("./etc/passwd")).unwrap(),
            PathBuf::from("etc/passwd")
        );
        assert!(safe_rootfs_relative(Path::new("/etc/passwd")).is_err());
        assert!(safe_rootfs_relative(Path::new("../evil")).is_err());
        assert!(safe_rootfs_relative(Path::new("etc/../../evil")).is_err());
    }

    #[test]
    fn import_rejects_an_archive_with_no_usable_entries() {
        let _home = crate::testenv::TempHome::new("importempty");
        let f = std::env::temp_dir().join(format!("wbox-imp-{}.tar", std::process::id()));
        {
            let mut b = tar::Builder::new(std::fs::File::create(&f).unwrap());
            b.finish().unwrap();
        }
        let m = format!(
            "{}",
            cmd_import(&["-t".to_string(), "e:v1".to_string(), f.to_string_lossy().to_string()])
                .unwrap_err()
        );
        assert!(m.contains("rootfs"), "{}", m);
        assert!(m.contains("wbox load"), "要指出带身份的归档该用 load：{}", m);
        let _ = std::fs::remove_file(&f);
    }
}
