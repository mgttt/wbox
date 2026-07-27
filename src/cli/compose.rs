//! `wbox compose`：`up` / `down` / `ps` 三个动词（`PRD.md` F9.14）。
//!
//! 这一层只做**编排**：读文件、定顺序、把每个 service 翻译成一条 `wbox run`
//! 的 argv 交给 [`super::run::cmd_run`]。容器怎么起、参数怎么校验，全部沿用
//! run 自己那一套——不另写一份，就不会出现"compose 起的容器和 run 起的行为
//! 不一样"这种最难查的偏差。

use crate::compose::{self, Project};
use crate::error::{Result, WboxError};

/// 默认文件名，按顺序探测（与 docker compose 一致）。
const DEFAULT_FILES: &[&str] = &["compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml"];

struct Options {
    file: Option<String>,
    project: Option<String>,
    detach: bool,
    rest: Vec<String>,
}

fn parse(args: &[String]) -> Result<(String, Options)> {
    let mut o = Options {
        file: None,
        project: None,
        detach: false,
        rest: Vec::new(),
    };
    let mut verb: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-f" | "--file" => o.file = Some(super::args::take_value(args, &mut i, "--file")?),
            "-p" | "--project-name" => {
                o.project = Some(super::args::take_value(args, &mut i, "--project-name")?)
            }
            "-d" | "--detach" => o.detach = true,
            other if other.starts_with('-') && verb.is_none() => {
                return Err(WboxError::args(format!("compose 未知选项 '{}'", other)));
            }
            other => {
                if verb.is_none() {
                    verb = Some(other.to_string());
                } else {
                    o.rest.push(other.to_string());
                }
            }
        }
        i += 1;
    }
    let verb = verb.ok_or_else(|| {
        WboxError::args("compose 缺少动词（支持 up / down / ps）")
    })?;
    Ok((verb, o))
}

/// 定位 compose 文件。
fn locate(file: Option<&str>) -> Result<std::path::PathBuf> {
    if let Some(f) = file {
        let p = std::path::PathBuf::from(f);
        if !p.is_file() {
            return Err(WboxError::args(format!("compose 文件 '{}' 不存在", f)));
        }
        return Ok(p);
    }
    for name in DEFAULT_FILES {
        let p = std::path::PathBuf::from(name);
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(WboxError::args(format!(
        "当前目录没有 compose 文件（找过 {}）；可用 -f 指定",
        DEFAULT_FILES.join(" / ")
    )))
}

/// 项目名：显式 `-p` 优先，否则用**文件所在目录名**（与 docker compose 一致）。
fn project_name(path: &std::path::Path, explicit: Option<&str>) -> String {
    if let Some(p) = explicit {
        return p.to_string();
    }
    path.canonicalize()
        .ok()
        .as_deref()
        .and_then(|p| p.parent().and_then(|d| d.file_name()))
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "wbox".to_string())
}

fn load(o: &Options) -> Result<Project> {
    let path = locate(o.file.as_deref())?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| WboxError::args(format!("读取 '{}' 失败：{}", path.display(), e)))?;
    let name = project_name(&path, o.project.as_deref());
    compose::parse(&text, &name)
}

pub fn cmd_compose(args: &[String]) -> Result<u32> {
    let (verb, o) = parse(args)?;
    match verb.as_str() {
        "up" => up(&o),
        "down" => down(&o),
        "ps" => ps(&o),
        other => Err(WboxError::args(format!(
            "未知 compose 动词 '{}'（支持 up / down / ps）",
            other
        ))),
    }
}

fn up(o: &Options) -> Result<u32> {
    let project = load(o)?;
    // 前台跑多个服务没有合理语义（谁的 stdio 接到终端？），故要求 -d。
    // 明说而不是偷偷加 -d——偷偷加会让用户以为命令挂了。
    if !o.detach {
        return Err(WboxError::args(
            "compose up 目前只支持 -d：多个服务同时前台运行没有确定的 stdio 归属；\
             起来后用 wbox logs <项目>_<服务> 看输出",
        ));
    }
    // 第一个服务持有网络，其余加入它（见 compose 模块文档的网络语义说明）。
    let owner = project
        .services
        .first()
        .map(|s| project.container_name(&s.name));
    for (idx, svc) in project.services.iter().enumerate() {
        let peer = if idx == 0 { None } else { owner.as_deref() };
        let argv = compose::run_args(&project, svc, peer, true);
        println!("wbox: 启动服务 '{}' → {}", svc.name, project.container_name(&svc.name));
        super::run::cmd_run(&argv)?;
    }
    println!("wbox: compose up 完成（项目 {}）", project.name);
    Ok(0)
}

fn down(o: &Options) -> Result<u32> {
    let project = load(o)?;
    // 逆序停：依赖方先走，被依赖的后走，与启动顺序相反。
    // 单个服务停不掉不该让整轮中断——down 的意图是"尽量都清掉"，
    // 一个已经手工删过的容器不该挡住其余的清理。
    let mut failed = 0u32;
    for svc in project.services.iter().rev() {
        let name = project.container_name(&svc.name);
        if let Err(e) = super::stop::cmd_stop(std::slice::from_ref(&name)) {
            eprintln!("wbox: 停止 '{}' 失败：{}", name, e);
            failed += 1;
            continue;
        }
        if let Err(e) = super::rm::cmd_rm(std::slice::from_ref(&name)) {
            eprintln!("wbox: 删除 '{}' 失败：{}", name, e);
            failed += 1;
        }
    }
    if failed > 0 {
        return Err(WboxError::args(format!(
            "compose down：{} 个服务未能完全清理（详见上面的说明）",
            failed
        )));
    }
    println!("wbox: compose down 完成（项目 {}）", project.name);
    Ok(0)
}

fn ps(o: &Options) -> Result<u32> {
    let project = load(o)?;
    println!("{:<20} {:<24} {:<20} 镜像", "服务", "容器", "状态");
    for svc in &project.services {
        let cname = project.container_name(&svc.name);
        let state = match crate::runstate::dir_for(&cname) {
            Ok(dir) if dir.exists() => {
                let base = match crate::runstate::liveness(&dir) {
                    crate::runstate::Liveness::Running => "running",
                    crate::runstate::Liveness::Exited => "exited",
                    crate::runstate::Liveness::Created => "created",
                };
                match crate::health::read_status(&dir) {
                    Some(h) => format!("{} ({})", base, h),
                    None => base.to_string(),
                }
            }
            // 没有状态目录 = 这个服务没起过。不说"exited"——那会让人以为
            // 它跑过又退了。
            _ => "-".to_string(),
        };
        println!("{:<20} {:<24} {:<20} {}", svc.name, cname, state, svc.image);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_verb_and_options() {
        let a: Vec<String> = ["-f", "x.yaml", "up", "-d"].iter().map(|s| s.to_string()).collect();
        let (v, o) = parse(&a).unwrap();
        assert_eq!(v, "up");
        assert_eq!(o.file.as_deref(), Some("x.yaml"));
        assert!(o.detach);

        let a: Vec<String> = ["-p", "proj", "down"].iter().map(|s| s.to_string()).collect();
        let (v, o) = parse(&a).unwrap();
        assert_eq!(v, "down");
        assert_eq!(o.project.as_deref(), Some("proj"));

        assert!(parse(&[]).is_err(), "缺动词应报错");
        assert!(parse(&["--bogus".to_string()]).is_err());
        assert!(parse(&["-f".to_string()]).is_err(), "缺取值应报错");
    }

    #[test]
    fn unknown_verb_lists_supported() {
        let e = cmd_compose(&["restart".to_string()]).unwrap_err();
        let m = format!("{}", e);
        assert!(m.contains("up / down / ps"), "{}", m);
    }

    #[test]
    fn missing_file_is_reported_with_search_list() {
        let e = locate(None).unwrap_err();
        let m = format!("{}", e);
        // 当前目录（仓库根）没有 compose 文件，应列出找过哪些名字
        assert!(m.contains("compose.yaml"), "{}", m);
        assert!(locate(Some("definitely-absent.yaml")).is_err());
    }

    /// 前台跑多服务没有确定的 stdio 归属，要明说而不是偷偷加 -d。
    #[test]
    fn up_without_detach_explains_instead_of_silently_detaching() {
        let dir = std::env::temp_dir().join(format!("wbox-compose-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("compose.yaml");
        std::fs::write(&f, "services:\n  a:\n    image: x\n").unwrap();
        let o = Options {
            file: Some(f.to_string_lossy().to_string()),
            project: Some("t".into()),
            detach: false,
            rest: Vec::new(),
        };
        let m = format!("{}", up(&o).unwrap_err());
        assert!(m.contains("-d"), "{}", m);
        assert!(m.contains("stdio"), "要说清为什么：{}", m);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_name_defaults_to_directory() {
        let dir = std::env::temp_dir().join(format!("wbox-cproj-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("compose.yaml");
        std::fs::write(&f, "services:\n  a:\n    image: x\n").unwrap();
        let n = project_name(&f, None);
        assert_eq!(n, dir.file_name().unwrap().to_string_lossy());
        assert_eq!(project_name(&f, Some("explicit")), "explicit");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
