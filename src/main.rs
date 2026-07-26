//! wbox —— portable Windows 进程容器（MVP）
//!
//! 隔离单元 = AppContainer（令牌隔离 + Low IL）+ Job Object（资源限额 + 生命周期收割）。
//! 默认不需要管理员权限、不需要 VT-x、不需要启用任何 Windows 可选功能。
//!
//! `wbox run` 有两类目标（见 backend/mod.rs）：
//! - 本地 Windows 可执行路径 → NativeBackend（AppContainer + Job 直接运行）；
//! - 已 pull 的 OCI 镜像引用（或带 `--pull`）→ BlinkBackend（wbox-linux 模拟，
//!   骨架；config.json 的 Env/Cmd/Entrypoint/WorkingDir 在此消费）。
//!
//! 退出码约定（SPEC §2 + OCI 扩展）：
//!   子进程退出码原样转发；wbox 自身错误：1=参数 2=profile 3=job 4=进程创建 5=registry/镜像。

mod backend;
mod error;
// OCI 镜像拉取：纯 Rust 依赖，跨平台可编译（Linux 沙箱用于实测拉取逻辑）。
mod oci;
// 以下模块直接调用 Win32 API，仅 Windows 可编译。
#[cfg(windows)]
mod acl;
#[cfg(windows)]
mod job;
#[cfg(windows)]
mod sandbox;
#[cfg(windows)]
mod token;

use backend::{Backend, BlinkBackend, Limits, RunSpec, RunTarget};
use error::WboxError;

const USAGE: &str = r#"wbox — portable Windows 进程容器（AppContainer + Job Object）

用法:
  wbox run [OPTIONS] -- <CMD> [ARGS...]            运行本地 Windows 程序
  wbox run [OPTIONS] <IMAGE> [-- <CMD> [ARGS...]]  运行已 pull 的 OCI 镜像（Linux 后端骨架）
  wbox image pull <REF> [--os linux] [--arch amd64] [--registry <HOST>] [-V]
  wbox image list
  wbox image show <REF>                            打印已 pull 镜像的 config 摘要
  wbox --help | -h
  wbox --version

选项:
  --name <NAME>     容器名（AppContainer profile 名），默认 "wbox-<pid>"
  --memory <MB>     每进程内存上限（MB），0 = 不限，默认 0
  --cpu-pct <N>     CPU 硬性百分比上限 1-100（Job CPU rate control），默认 0 = 不限
  --max-procs <N>   最大进程数，默认 0 = 不限
  --allow-network   授予 INTERNET_CLIENT capability（默认不授予任何网络能力）
  --no-network      显式声明不授予网络（默认行为，预留）
  --workdir <DIR>   容器工作目录（"镜像根"），默认当前目录（仅原生模式）
  --keep-profile    退出后保留 AppContainer profile（默认删除）
  --interactive     连接 stdio（当前默认且唯一支持的模式；--detach 预留）
  --pull            run 目标为镜像时，本地无缓存则先 pull
  --env-pass-all    继承完整宿主环境（默认仅白名单；BLINK_*/WBOX_* 保留键始终不透传）
  -V, --verbose     打印隔离配置摘要

镜像模式说明:
  位置参数能解析为镜像引用（如 ubuntu:24.04）且已在本地缓存（或带 --pull）时，
  视为镜像目标：自动按 docker 规则合并 config.json 的 Entrypoint/Cmd，
  注入 Env，rootfs 作为工作目录。镜像经 wbox-linux（blink 移植，开发中）
  模拟执行，未就绪时会得到明确错误。

示例:
  wbox run --memory 256 --cpu-pct 50 -- cmd.exe /c echo hello
  wbox run --name test1 --workdir C:\img\app --allow-network -- myapp.exe
  wbox run ubuntu:24.04 -- bash
  wbox run --pull alpine:3.20 -- sh -c "echo hi"
"#;

/// `run` 子命令的全部参数（跨平台结构，纯逻辑可在 Linux 沙箱单测）。
#[derive(Debug)]
struct RunOptions {
    name: Option<String>,
    limits: Limits,
    allow_network: bool,
    keep_profile: bool,
    workdir: Option<String>,
    verbose: bool,
    /// 本地无缓存时先 pull（镜像模式）
    pull: bool,
    /// 继承完整宿主环境（默认仅白名单；保留键始终不透传）
    env_pass_all: bool,
    /// 第一个位置参数：镜像引用候选 或 本地命令首词
    positional: Option<String>,
    /// `--` 之后（或未写 `--` 时 positional 之后）的命令与参数
    cmd: Vec<String>,
}

fn main() {
    let code = match real_main() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wbox: 错误: {}", e);
            e.exit_code()
        }
    };
    // 子进程退出码按位原样转发（Windows 退出码为 u32，如 0xC000xxxx）。
    std::process::exit(code as i32);
}

fn real_main() -> error::Result<u32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("run") => cmd_run(&args[1..]),
        Some("image") => cmd_image(&args[1..]),
        Some("--help") | Some("-h") | Some("help") => {
            print!("{}", USAGE);
            Ok(0)
        }
        Some("--version") => {
            println!("wbox {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        Some(other) => Err(WboxError::args(format!(
            "未知子命令 '{}'。用法见 wbox --help",
            other
        ))),
        None => {
            print!("{}", USAGE);
            Err(WboxError::args("缺少子命令（run / image）"))
        }
    }
}

/// 手写参数解析：支持 `--opt value`、`--flag`、至多一个位置参数（镜像引用
/// 候选 / 本地命令首词）与 `--` 分隔的命令行。
fn parse_run_args(args: &[String]) -> error::Result<RunOptions> {
    let mut opts = RunOptions {
        name: None,
        limits: Limits::default(),
        allow_network: false,
        keep_profile: false,
        workdir: None,
        verbose: false,
        pull: false,
        env_pass_all: false,
        positional: None,
        cmd: Vec::new(),
    };

    let mut i = 0;
    let mut saw_dashdash = false;
    while i < args.len() {
        let a = &args[i];
        if saw_dashdash {
            opts.cmd.push(a.clone());
            i += 1;
            continue;
        }
        match a.as_str() {
            "--" => saw_dashdash = true,
            "--name" => {
                opts.name = Some(take_value(args, &mut i, "--name")?);
            }
            "--memory" => {
                let v = take_value(args, &mut i, "--memory")?;
                opts.limits.memory_mb = v
                    .parse::<u64>()
                    .map_err(|_| WboxError::args(format!("--memory 需为非负整数（MB），得到 '{}'", v)))?;
            }
            "--cpu-pct" => {
                let v = take_value(args, &mut i, "--cpu-pct")?;
                let n = v
                    .parse::<u32>()
                    .map_err(|_| WboxError::args(format!("--cpu-pct 需为整数，得到 '{}'", v)))?;
                if n > 100 {
                    return Err(WboxError::args("--cpu-pct 取值范围为 0-100（0 = 不限）"));
                }
                opts.limits.cpu_pct = n;
            }
            "--max-procs" => {
                let v = take_value(args, &mut i, "--max-procs")?;
                opts.limits.max_procs = v
                    .parse::<u32>()
                    .map_err(|_| WboxError::args(format!("--max-procs 需为非负整数，得到 '{}'", v)))?;
            }
            "--allow-network" => opts.allow_network = true,
            "--no-network" => opts.allow_network = false, // 显式默认，预留
            "--keep-profile" => opts.keep_profile = true,
            "--interactive" => {} // v1 唯一支持的模式，接受并忽略
            "--pull" => opts.pull = true,
            "--env-pass-all" => opts.env_pass_all = true,
            "--workdir" => {
                opts.workdir = Some(take_value(args, &mut i, "--workdir")?);
            }
            "-V" | "--verbose" => opts.verbose = true,
            other if other.starts_with('-') => {
                return Err(WboxError::args(format!("未知选项 '{}'", other)));
            }
            // 第一个非选项参数 = 位置参数（镜像引用候选 / 本地命令首词）；
            // 其后未写 "--" 的非选项参数容错并入命令（兼容 v1 写法）。
            _ => {
                if opts.positional.is_none() {
                    opts.positional = Some(a.clone());
                } else {
                    opts.cmd.push(a.clone());
                }
            }
        }
        i += 1;
    }

    Ok(opts)
}

/// 取 `--opt <value>` 形式的值；i 指向选项本身，成功后移动到值。
fn take_value(args: &[String], i: &mut usize, opt: &str) -> error::Result<String> {
    if *i + 1 >= args.len() {
        return Err(WboxError::args(format!("选项 '{}' 缺少参数值", opt)));
    }
    *i += 1;
    Ok(args[*i].clone())
}

/// 由 RunOptions 组装 RunSpec 的公共部分。
fn make_spec(opts: &RunOptions, workdir: std::path::PathBuf, cmd: Vec<String>, env: Vec<(String, String)>) -> RunSpec {
    RunSpec {
        name: opts
            .name
            .clone()
            .unwrap_or_else(|| format!("wbox-{}", std::process::id())),
        limits: opts.limits,
        allow_network: opts.allow_network,
        keep_profile: opts.keep_profile,
        workdir,
        cmd,
        env,
        verbose: opts.verbose,
        env_pass_all: opts.env_pass_all,
    }
}

fn cmd_run(args: &[String]) -> error::Result<u32> {
    let opts = parse_run_args(args)?;

    // 判别目标：镜像引用（已 pull 或 --pull）vs 本地可执行路径。
    let target = backend::classify_target(opts.positional.as_deref(), opts.pull, oci::is_pulled)?;

    match target {
        RunTarget::Native => {
            // 本地命令 = 位置参数（若有）+ `--` 后参数
            let mut cmd: Vec<String> = Vec::new();
            if let Some(p) = opts.positional.clone() {
                cmd.push(p);
            }
            cmd.extend(opts.cmd.iter().cloned());
            if cmd.is_empty() {
                return Err(WboxError::args("缺少要执行的命令（-- <CMD> [ARGS...]）"));
            }
            run_native(&opts, cmd)
        }
        RunTarget::Image(iref) => run_image(&opts, iref),
    }
}

/// 原生模式：本地 Windows 程序（仅 Windows 可执行）。
#[cfg(windows)]
fn run_native(opts: &RunOptions, cmd: Vec<String>) -> error::Result<u32> {
    let workdir = match &opts.workdir {
        Some(d) => std::path::PathBuf::from(d),
        None => std::env::current_dir()
            .map_err(|e| WboxError::args(format!("获取当前目录失败：{}", e)))?,
    };
    let spec = make_spec(opts, workdir, cmd, Vec::new());
    let backend = backend::NativeBackend;
    let prepared = backend.prepare(&spec)?;
    backend.spawn(&spec, &prepared)
}

/// 非 Windows 平台：原生模式不可用（隔离原语为 Win32 API）。
#[cfg(not(windows))]
fn run_native(_opts: &RunOptions, _cmd: Vec<String>) -> error::Result<u32> {
    Err(WboxError::args(
        "run 原生模式仅在 Windows 上可用（AppContainer/Job Object 为 Win32 原语）",
    ))
}

/// 镜像模式：消费 config.json，经 BlinkBackend（wbox-linux 模拟）执行。
fn run_image(opts: &RunOptions, iref: oci::ImageRef) -> error::Result<u32> {
    let dir = oci::image_dir(&iref)?;
    if !dir.join("rootfs").is_dir() {
        // classify 保证要么已缓存、要么带 --pull；走到这里必然是 --pull 未命中
        oci::pull(&iref.repo_tag(), "linux", "amd64", None, opts.verbose)?;
        if !dir.join("rootfs").is_dir() {
            return Err(WboxError::registry(format!(
                "pull 后仍未找到镜像缓存 '{}'，无法运行",
                dir.display()
            )));
        }
    }

    // 消费 image config：Env 注入、Entrypoint/Cmd 按 docker 规则合并、
    // WorkingDir 记录（guest 路径映射由 wbox-linux 侧落地，当前仅展示）。
    let img_cfg = oci::config::ImageConfig::load(&dir)?;
    let (merged, env) = match &img_cfg {
        Some(c) => (c.merged_command(&opts.cmd), c.env.clone()),
        None => (opts.cmd.clone(), Vec::new()),
    };
    if merged.is_empty() {
        return Err(WboxError::args(format!(
            "镜像 '{}' 未声明 Entrypoint/Cmd，请在 `--` 后显式给出要执行的命令",
            iref.repo_tag()
        )));
    }
    if opts.verbose {
        if let Some(c) = &img_cfg {
            if let Some(wd) = &c.working_dir {
                println!("wbox: 镜像 WorkingDir = {}（guest 路径，由 wbox-linux 映射）", wd);
            }
        }
    }

    let spec = make_spec(opts, dir.join("rootfs"), merged, env);
    let backend = BlinkBackend;
    let prepared = backend.prepare(&spec)?;
    backend.spawn(&spec, &prepared)
}

/// `wbox image` 子命令：pull / list / show。
fn cmd_image(args: &[String]) -> error::Result<u32> {
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
fn cmd_image_show(args: &[String]) -> error::Result<u32> {
    let mut image_ref: Option<String> = None;
    for a in args {
        if a.starts_with('-') {
            return Err(WboxError::args(format!("未知选项 '{}'", a)));
        }
        if image_ref.is_some() {
            return Err(WboxError::args(format!("多余的参数 '{}'", a)));
        }
        image_ref = Some(a.clone());
    }
    let image_ref = image_ref.ok_or_else(|| WboxError::args("image show 缺少镜像引用"))?;

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
fn cmd_image_pull(args: &[String]) -> error::Result<u32> {
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
            "--os" => os = take_value(args, &mut i, "--os")?,
            "--arch" => arch = take_value(args, &mut i, "--arch")?,
            "--registry" => registry = Some(take_value(args, &mut i, "--registry")?),
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

    fn parse(args: &[&str]) -> error::Result<RunOptions> {
        let v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_run_args(&v)
    }

    // ---- 基础解析 ----

    #[test]
    fn parse_native_dashdash_command() {
        let o = parse(&["--memory", "256", "--cpu-pct", "50", "--", "cmd.exe", "/c", "echo"]).unwrap();
        assert_eq!(o.limits.memory_mb, 256);
        assert_eq!(o.limits.cpu_pct, 50);
        assert_eq!(o.positional, None);
        assert_eq!(o.cmd, vec!["cmd.exe", "/c", "echo"]);
        assert!(!o.pull);
    }

    #[test]
    fn parse_positional_and_dashdash() {
        let o = parse(&["ubuntu:24.04", "--", "bash", "-l"]).unwrap();
        assert_eq!(o.positional.as_deref(), Some("ubuntu:24.04"));
        assert_eq!(o.cmd, vec!["bash", "-l"]);
    }

    #[test]
    fn parse_positional_without_dashdash_collects_command() {
        // docker 风格：镜像后直接跟 cmd（容错并入，不写 -- 也行）
        let o = parse(&["ubuntu:24.04", "bash"]).unwrap();
        assert_eq!(o.positional.as_deref(), Some("ubuntu:24.04"));
        assert_eq!(o.cmd, vec!["bash"]);
        // v1 风格：无 -- 的本地命令
        let o = parse(&["cmd.exe", "/c", "echo"]).unwrap();
        assert_eq!(o.positional.as_deref(), Some("cmd.exe"));
        assert_eq!(o.cmd, vec!["/c", "echo"]);
    }

    #[test]
    fn parse_pull_flag_and_misc() {
        let o = parse(&["--pull", "-V", "--name", "t1", "--keep-profile", "alpine:3.20"]).unwrap();
        assert!(o.pull && o.verbose && o.keep_profile);
        assert_eq!(o.name.as_deref(), Some("t1"));
        assert_eq!(o.positional.as_deref(), Some("alpine:3.20"));
    }

    #[test]
    fn parse_errors() {
        // 缺值
        assert!(parse(&["--memory"]).is_err());
        // 非法数字
        assert!(parse(&["--memory", "-1", "--", "x"]).is_err());
        assert!(parse(&["--cpu-pct", "101", "--", "x"]).is_err());
        // 未知选项
        assert!(parse(&["--bogus", "--", "x"]).is_err());
    }

    // ---- 目标判别后的命令组装（Native 分支）----

    #[test]
    fn native_command_combines_positional_and_dashdash() {
        // 未 pull 的字符串全部回退 Native：positional 必须并入 cmd，
        // 否则 `wbox run cmd.exe /c echo` 会丢掉 cmd.exe
        let o = parse(&["cmd.exe", "/c", "echo", "hi"]).unwrap();
        let mut cmd: Vec<String> = Vec::new();
        if let Some(p) = o.positional.clone() {
            cmd.push(p);
        }
        cmd.extend(o.cmd.iter().cloned());
        assert_eq!(cmd, vec!["cmd.exe", "/c", "echo", "hi"]);
    }

    // ---- 其余选项解析 ----

    #[test]
    fn parse_env_pass_all_and_network_flags() {
        let o = parse(&["--env-pass-all", "--allow-network", "--workdir", r"C:\app", "--", "x"]).unwrap();
        assert!(o.env_pass_all);
        assert!(o.allow_network);
        assert_eq!(o.workdir.as_deref(), Some(r"C:\app"));
        // --no-network 显式回退默认
        let o = parse(&["--allow-network", "--no-network", "--", "x"]).unwrap();
        assert!(!o.allow_network);
    }

    #[test]
    fn parse_max_procs_and_numeric_bounds() {
        let o = parse(&["--max-procs", "64", "--", "x"]).unwrap();
        assert_eq!(o.limits.max_procs, 64);
        assert!(parse(&["--max-procs", "-1", "--", "x"]).is_err());
        assert!(parse(&["--max-procs", "abc", "--", "x"]).is_err());
        assert!(parse(&["--cpu-pct", "100", "--", "x"]).is_ok());
        // 选项缺值一律报错
        for args in [&["--name"][..], &["--cpu-pct"][..], &["--max-procs"][..], &["--workdir"][..]] {
            assert!(parse(args).is_err(), "{:?}", args);
        }
    }

    #[test]
    fn parse_interactive_flag_accepted() {
        let o = parse(&["--interactive", "--", "x"]).unwrap();
        assert_eq!(o.cmd, vec!["x"]);
    }

    // ---- image 子命令参数错误 ----

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

    // ---- 集成：临时 HOME 下的假缓存全链（list → show → run prepare）----

    /// 临时 HOME 脚手架：构造期间把 HOME 指向独立临时目录，Drop 时恢复并清理。
    /// （cache_root 优先 USERPROFILE，故同时摘掉它；仅测试进程内生效。）
    struct TempHome {
        dir: std::path::PathBuf,
        saved_home: Option<std::ffi::OsString>,
        saved_userprofile: Option<std::ffi::OsString>,
    }

    impl TempHome {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "wbox-test-home-{}-{}",
                std::process::id(),
                tag
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let saved_home = std::env::var_os("HOME");
            let saved_userprofile = std::env::var_os("USERPROFILE");
            std::env::set_var("HOME", &dir);
            std::env::remove_var("USERPROFILE");
            Self { dir, saved_home, saved_userprofile }
        }

        /// 在缓存布局中安放一个假镜像（rootfs + 元数据），返回缓存目录。
        fn plant_fake_image(&self, registry: &str, name_flat: &str, tag: &str) -> std::path::PathBuf {
            let dir = self
                .dir
                .join(".wbox/images")
                .join(registry)
                .join(name_flat)
                .join(tag);
            std::fs::create_dir_all(dir.join("rootfs/bin")).unwrap();
            std::fs::write(dir.join("rootfs/bin/sh"), b"fake").unwrap();
            std::fs::write(dir.join("manifest.json"), b"{}").unwrap();
            std::fs::write(dir.join("layers.json"), r#"["sha256:l1","sha256:l2"]"#).unwrap();
            std::fs::write(
                dir.join("config.json"),
                r#"{"config":{"Env":["PATH=/usr/bin","APP_TOKEN=hunter2"],"Cmd":["-l"],"Entrypoint":["/bin/sh"],"WorkingDir":"/root"}}"#,
            )
            .unwrap();
            dir
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.saved_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match &self.saved_userprofile {
                Some(v) => std::env::set_var("USERPROFILE", v),
                None => std::env::remove_var("USERPROFILE"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn integration_list_show_run_prepare_chain() {
        let home = TempHome::new("chain");
        home.plant_fake_image("registry-1.docker.io", "library_fake", "latest");

        // 1. list：扫描到已缓存镜像（返回 Ok 即不报错；输出人工可查）
        oci::list().unwrap();

        // 2. show：打印 config 摘要（含脱敏路径）
        cmd_image_show(&["fake:latest".to_string()]).unwrap();

        // 3. classify：按真实缓存判定（与 cmd_run 相同的闭包）
        let target = backend::classify_target(Some("fake"), false, oci::is_pulled).unwrap();
        let iref = match target {
            backend::RunTarget::Image(r) => r,
            other => panic!("已缓存镜像必须判为 Image，得到 {:?}", other),
        };
        assert_eq!(iref.repo, "library/fake");

        // 4. run-prepare：config 合并 + BlinkBackend 执行计划（不 spawn）
        let dir = oci::image_dir(&iref).unwrap();
        let cfg = oci::config::ImageConfig::load(&dir).unwrap().unwrap();
        let merged = cfg.merged_command(&[]); // 无显式 cmd：Entrypoint + Cmd
        assert_eq!(merged, vec!["/bin/sh", "-l"]);
        assert_eq!(cfg.working_dir.as_deref(), Some("/root"));

        let fake_exe = std::env::current_exe().unwrap();
        // WBOX_LINUX 环境变量（blink::LINUX_EXE_ENV，模块私有故此处用字面量）
        std::env::set_var("WBOX_LINUX", &fake_exe);
        let spec = backend::RunSpec {
            name: "t".to_string(),
            limits: Default::default(),
            allow_network: false,
            keep_profile: false,
            workdir: dir.join("rootfs"),
            cmd: merged,
            env: cfg.env.clone(),
            verbose: false,
            env_pass_all: false,
        };
        let prepared = backend::BlinkBackend.prepare(&spec).unwrap();
        std::env::remove_var("WBOX_LINUX");
        // 执行计划：wbox-linux + 合并命令；BLINK_PREFIX 指向 rootfs
        assert_eq!(prepared.cmd[0], fake_exe.to_string_lossy());
        assert_eq!(&prepared.cmd[1..], &["/bin/sh", "-l"]);
        assert!(prepared
            .env
            .iter()
            .any(|(k, v)| k == "BLINK_PREFIX" && v == &dir.join("rootfs").to_string_lossy()));
        // 镜像 Env 注入，敏感键仍在（脱敏只发生在打印路径）
        assert!(prepared.env.iter().any(|(k, v)| k == "APP_TOKEN" && v == "hunter2"));
        // resolv.conf 已注入（rootfs 原本没有）
        let resolv = std::fs::read_to_string(dir.join("rootfs/etc/resolv.conf")).unwrap();
        assert!(resolv.contains("nameserver"), "{}", resolv);
    }

    #[test]
    fn integration_uncached_then_plant_then_classify() {
        // 未缓存 → Native；构造缓存后 → Image（classify 实时看磁盘）
        let home = TempHome::new("promote");
        assert_eq!(
            backend::classify_target(Some("fake"), false, oci::is_pulled).unwrap(),
            backend::RunTarget::Native
        );
        home.plant_fake_image("registry-1.docker.io", "library_fake", "latest");
        match backend::classify_target(Some("fake"), false, oci::is_pulled).unwrap() {
            backend::RunTarget::Image(_) => {}
            other => panic!("期望 Image，得到 {:?}", other),
        }
    }

    #[test]
    fn integration_pull_hello_world_or_skip_when_network_unreachable() {
        // 真实网络拉取：registry 不可达时 SKIP（不 fail）；可达时校验全链落盘。
        let _home = TempHome::new("realpull"); // HOME 指向临时目录，Drop 时恢复
        let r = oci::ImageRef::parse("hello-world", None).unwrap();
        match oci::pull("hello-world", "linux", "amd64", None, false) {
            Ok(()) => {
                let dir = oci::image_dir(&r).unwrap();
                assert!(dir.join("manifest.json").is_file());
                assert!(dir.join("config.json").is_file());
                assert!(dir.join("layers.json").is_file());
                assert!(dir.join("rootfs").is_dir());
                oci::list().unwrap();
                cmd_image_show(&["hello-world".to_string()]).unwrap();
            }
            Err(e) => {
                eprintln!("SKIP：registry 不可达（{}），真实 pull 链路不判失败", e);
            }
        }
    }
}
