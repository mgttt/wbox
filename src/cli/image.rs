//! `wbox image` 子命令：pull / list / show。

use crate::error::{Result, WboxError};
use crate::backend;
use crate::oci;

/// `wbox image` 子命令：pull / list / show。
pub fn cmd_image(args: &[String]) -> Result<u32> {
    match args.first().map(|s| s.as_str()) {
        Some("pull") => cmd_image_pull(&args[1..]),
        Some("list") | Some("ls") => oci::list(),
        Some("show") => cmd_image_show(&args[1..]),
        Some(other) => Err(WboxError::args(format!(
            "未知 image 子命令 '{}'（支持 pull / list / show）",
            other
        ))),
        None => Err(WboxError::args("image 缺少子命令（pull / list / show）")),
    }
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
fn cmd_image_pull(args: &[String]) -> Result<u32> {
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


#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::TempHome;

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
