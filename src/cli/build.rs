//! `wbox build`：Dockerfile 子集构建（`PRD.md` F9.3）。

use crate::build::{run_build, BuildOptions};
use crate::error::{Result, WboxError};
use std::path::PathBuf;

pub fn cmd_build(args: &[String]) -> Result<u32> {
    let mut tag: Option<String> = None;
    let mut dockerfile: Option<String> = None;
    let mut context: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-t" | "--tag" => tag = Some(super::args::take_value(args, &mut i, "--tag")?),
            "-f" | "--file" => {
                dockerfile = Some(super::args::take_value(args, &mut i, "--file")?)
            }
            other if other.starts_with('-') => {
                return Err(WboxError::args(format!(
                    "build: 未知参数 '{}'（用法：wbox build -t NAME[:TAG] [-f Dockerfile] <上下文目录>）",
                    other
                )))
            }
            other => {
                if context.is_some() {
                    return Err(WboxError::args("build: 只能指定一个构建上下文目录"));
                }
                context = Some(other.to_string());
            }
        }
        i += 1;
    }
    let tag = tag.ok_or_else(|| {
        WboxError::args("build: 缺少 -t NAME[:TAG]（构建结果没有名字就无法 run）")
    })?;
    let context = PathBuf::from(context.unwrap_or_else(|| ".".to_string()));
    if !context.is_dir() {
        return Err(WboxError::args(format!(
            "构建上下文 '{}' 不是目录",
            context.display()
        )));
    }
    let dockerfile = dockerfile
        .map(PathBuf::from)
        .unwrap_or_else(|| context.join("Dockerfile"));
    if !dockerfile.is_file() {
        return Err(WboxError::args(format!(
            "找不到 Dockerfile '{}'（用 -f 指定）",
            dockerfile.display()
        )));
    }
    run_build(&BuildOptions {
        context,
        dockerfile,
        tag,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn requires_tag_and_valid_paths() {
        let home = TempHome::new("build-args");
        let ctx = home.dir.join("ctx");
        std::fs::create_dir_all(&ctx).unwrap();
        let c = ctx.to_string_lossy().to_string();
        // 缺 -t：构建结果没有名字就无法 run，必须报错而不是给个默认名
        let e = cmd_build(std::slice::from_ref(&c)).unwrap_err();
        assert!(format!("{}", e).contains("缺少 -t"), "{}", e);
        // 上下文不是目录
        assert!(cmd_build(&["-t".into(), "x".into(), "/no/such/dir".into()]).is_err());
        // 上下文里没有 Dockerfile
        let e = cmd_build(&["-t".into(), "x".into(), c.clone()]).unwrap_err();
        assert!(format!("{}", e).contains("找不到 Dockerfile"), "{}", e);
        assert!(cmd_build(&["--bogus".into()]).is_err());
    }
}
