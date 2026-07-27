//! `wbox image` 子命令：pull / list / show / inspect / rm。

use crate::error::{Result, WboxError};
use crate::backend;
use crate::oci;

/// `wbox image` 子命令：pull / list / show / inspect / rm。
pub fn cmd_image(args: &[String]) -> Result<u32> {
    match args.first().map(|s| s.as_str()) {
        Some("pull") => cmd_image_pull(&args[1..]),
        Some("push") => cmd_image_push(&args[1..]),
        Some("list") | Some("ls") => cmd_image_list(&args[1..]),
        Some("show") => cmd_image_show(&args[1..]),
        Some("inspect") => super::inspect::cmd_image_inspect(&args[1..]),
        Some("rm") => cmd_image_rm(&args[1..]),
        Some(other) => Err(WboxError::args(format!(
            "未知 image 子命令 '{}'（支持 pull / push / list / show / inspect / rm）",
            other
        ))),
        None => Err(WboxError::args(
            "image 缺少子命令（pull / push / list / show / inspect / rm）",
        )),
    }
}

/// `wbox image list` / `image ls` / 顶级 `images` 的统一入口。
pub(super) fn cmd_image_list(args: &[String]) -> Result<u32> {
    if let Some(other) = args.first() {
        return Err(WboxError::args(format!(
            "image list 不支持参数 '{}'",
            other
        )));
    }
    oci::list()
}

/// 删除镜像的本地缓存目录（`rm` 的纯操作部分，与确认提示分离以便测试）。
/// 返回被删除的目录路径；未缓存时报"未 pull"错误。
fn remove_cached_image(iref: &oci::ImageRef) -> Result<std::path::PathBuf> {
    let dir = oci::image_dir(iref)?;
    if !dir.is_dir() {
        return Err(WboxError::registry(format!(
            "镜像 '{}' 未 pull（缓存目录 '{}' 不存在），无需删除",
            iref.repo_tag(),
            dir.display()
        )));
    }
    std::fs::remove_dir_all(&dir)
        .map_err(|e| WboxError::registry(format!("删除缓存目录 '{}' 失败：{}", dir.display(), e)))?;
    Ok(dir)
}

/// `wbox image rm <REF>`：删除已 pull 镜像的本地缓存。
/// 与 Docker/Podman 一致默认直接删除；`-f/--force` 和旧 `-y/--yes`
/// 作为兼容选项接受。当前尚无镜像依赖跟踪，因此 force 与默认行为相同。
pub(super) fn cmd_image_rm(args: &[String]) -> Result<u32> {
    let mut positional: Vec<String> = Vec::new();
    for a in args {
        match a.as_str() {
            "--force" | "-f" | "--yes" | "-y" => {}
            other if other.starts_with('-') => {
                return Err(WboxError::args(format!("未知选项 '{}'", other)));
            }
            _ => positional.push(a.clone()),
        }
    }
    let image_ref = super::args::take_single_positional(&positional, "image rm 缺少镜像引用")?;
    let iref = oci::ImageRef::parse(&image_ref, None)?;

    let dir = remove_cached_image(&iref)?;
    println!("wbox: 已删除 {}（{}）", iref.repo_tag(), dir.display());
    Ok(0)
}

/// `wbox image show <REF>`：打印已 pull 镜像的 config.json 摘要。
pub(crate) fn cmd_image_show(args: &[String]) -> Result<u32> {
    let image_ref = super::args::take_single_positional(args, "image show 缺少镜像引用")?;

    let iref = oci::ImageRef::parse(&image_ref, None)?;
    let dir = oci::image_dir(&iref)?;
    if !dir.is_dir() {
        return Err(WboxError::registry(format!(
            "镜像 '{}' 未 pull（缓存目录 '{}' 不存在）",
            iref.repo_tag(),
            dir.display()
        )));
    }

    println!("镜像      : {}", iref.repo_tag());
    println!("registry  : {}", iref.registry);
    println!("缓存目录  : {}", dir.display());
    println!(
        "rootfs    : {}",
        if dir.join("rootfs").is_dir() {
            "已解包"
        } else {
            "缺失（缓存不完整，请重新 pull）"
        }
    );

    match oci::config::ImageConfig::load(&dir)? {
        Some(c) => {
            println!("config.json 摘要:");
            println!(
                "  Entrypoint : {}",
                if c.entrypoint.is_empty() {
                    "（未设置）".to_string()
                } else {
                    format!("{:?}", c.entrypoint)
                }
            );
            println!(
                "  Cmd        : {}",
                if c.cmd.is_empty() {
                    "（未设置）".to_string()
                } else {
                    format!("{:?}", c.cmd)
                }
            );
            println!(
                "  WorkingDir : {}",
                c.working_dir.as_deref().unwrap_or("（未设置，默认 /）")
            );
            if c.env.is_empty() {
                println!("  Env        : （未设置）");
            } else {
                println!("  Env        :");
                for (k, v) in &c.env {
                    // 键名敏感的值脱敏（防镜像内嵌凭证经 show 输出泄露）
                    println!("    {}={}", k, backend::env::redact_value(k, v));
                }
            }
        }
        None => println!("config.json : 不存在（缓存不完整）"),
    }
    Ok(0)
}

/// 解析并执行 `image pull <ref> [--os ..] [--arch ..] [--registry ..] [-V]`。
pub(super) fn cmd_image_pull(args: &[String]) -> Result<u32> {
    let mut image_ref: Option<String> = None;
    // 默认拉 linux/amd64：Windows 进程容器无法运行 Linux 二进制，
    // rootfs 主要用于工具链/资源文件提取与调试，故默认与宿主解耦。
    let mut os = "linux".to_string();
    let mut arch = "amd64".to_string();
    let mut registry: Option<String> = None;
    let mut verbose = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--os" => os = super::args::take_value(args, &mut i, "--os")?,
            "--arch" => arch = super::args::take_value(args, &mut i, "--arch")?,
            "--registry" => registry = Some(super::args::take_value(args, &mut i, "--registry")?),
            "-V" | "--verbose" => verbose = true,
            other if other.starts_with('-') => {
                return Err(WboxError::args(format!("未知选项 '{}'", other)));
            }
            other => {
                if image_ref.is_some() {
                    return Err(WboxError::args(format!("多余的参数 '{}'", other)));
                }
                image_ref = Some(other.to_string());
            }
        }
        i += 1;
    }
    let image_ref = image_ref.ok_or_else(|| WboxError::args("image pull 缺少镜像引用"))?;
    oci::pull(&image_ref, &os, &arch, registry.as_deref(), verbose)?;
    Ok(0)
}

/// `wbox push <IMAGE>` / `wbox image push <IMAGE>`（PRD F9.13）。
pub(super) fn cmd_image_push(args: &[String]) -> Result<u32> {
    let mut image_ref: Option<String> = None;
    let mut registry: Option<String> = None;
    let mut verbose = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--registry" => registry = Some(super::args::take_value(args, &mut i, "--registry")?),
            "-V" | "--verbose" => verbose = true,
            other if other.starts_with('-') => {
                return Err(WboxError::args(format!("未知选项 '{}'", other)));
            }
            other => {
                if image_ref.is_some() {
                    return Err(WboxError::args(format!("多余的参数 '{}'", other)));
                }
                image_ref = Some(other.to_string());
            }
        }
        i += 1;
    }
    let image_ref = image_ref.ok_or_else(|| WboxError::args("push 缺少镜像引用"))?;
    oci::push::push(&image_ref, registry.as_deref(), verbose)?;
    Ok(0)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn image_subcommand_arg_errors() {
        // 缺子命令 / 未知子命令
        assert!(cmd_image(&[]).is_err());
        assert!(cmd_image(&["bogus".to_string()]).is_err());
        // pull 缺引用 / 未知选项 / 多余参数
        assert!(cmd_image_pull(&[]).is_err());
        assert!(cmd_image_pull(&["--bogus".to_string(), "x".to_string()]).is_err());
        assert!(cmd_image_pull(&["a".to_string(), "b".to_string()]).is_err());
        // show 缺引用 / 多余参数 / 未知选项
        assert!(cmd_image_show(&[]).is_err());
        assert!(cmd_image_show(&["a".to_string(), "b".to_string()]).is_err());
        assert!(cmd_image_show(&["-V".to_string()]).is_err());
        // rm 缺引用 / 多余参数 / 未知选项
        assert!(cmd_image_rm(&[]).is_err());
        assert!(cmd_image_rm(&["a".to_string(), "b".to_string()]).is_err());
        assert!(cmd_image_rm(&["--bogus".to_string(), "x".to_string()]).is_err());
    }

    #[test]
    fn image_ls_alias_uses_strict_list_parser() {
        let _home = TempHome::new("image-ls");
        assert_eq!(cmd_image(&["ls".to_string()]).unwrap(), 0);
        assert!(cmd_image(&["ls".to_string(), "--all".to_string()]).is_err());
    }

    #[test]
    fn image_rm_removes_planted_cache_by_default() {
        let home = TempHome::new("rm-yes");
        let dir = home.plant_fake_image("registry-1.docker.io", "library_fake", "latest");
        assert!(dir.is_dir());
        cmd_image_rm(&["fake:latest".to_string()]).unwrap();
        assert!(!dir.exists());
        drop(home);
    }

    #[test]
    fn image_rm_uncached_ref_is_registry_error() {
        let home = TempHome::new("rm-uncached");
        let e = cmd_image_rm(&["nope:0.1".to_string(), "--force".to_string()]).unwrap_err();
        assert!(format!("{}", e).contains("未 pull"), "{}", e);
        drop(home);
    }

    #[test]
    fn remove_cached_image_returns_deleted_dir() {
        let home = TempHome::new("rm-fn");
        let dir = home.plant_fake_image("registry-1.docker.io", "library_fake", "latest");
        let iref = oci::ImageRef::parse("fake:latest", None).unwrap();
        let removed = remove_cached_image(&iref).unwrap();
        assert_eq!(removed, dir);
        assert!(!dir.exists());
        // 再删一次 → 未 pull 错误（幂等保护）
        assert!(remove_cached_image(&iref).is_err());
        drop(home);
    }

    #[test]
    fn image_show_uncached_ref_is_registry_error() {
        // 未 pull 的镜像 show：报"未 pull"（退出码 5 = registry 类）
        let home = TempHome::new("show-uncached");
        let e = cmd_image_show(&["definitely-not-pulled-image:0.0".to_string()]).unwrap_err();
        assert!(format!("{}", e).contains("未 pull"), "{}", e);
        drop(home);
    }
}
