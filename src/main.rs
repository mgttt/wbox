//! wbox —— portable Windows 进程容器（MVP）
//!
//! 隔离单元 = AppContainer（令牌隔离 + Low IL）+ Job Object（资源限额 + 生命周期收割）。
//! 默认不需要管理员权限、不需要 VT-x、不需要启用任何 Windows 可选功能。
//!
//! 退出码约定（SPEC §2 + OCI 扩展）：
//!   子进程退出码原样转发；wbox 自身错误：1=参数 2=profile 3=job 4=进程创建 5=registry/镜像。

mod error;
// OCI 镜像拉取：纯 Rust 依赖，跨平台可编译（Linux 沙箱用于实测拉取逻辑）。
mod oci;
// 以下模块直接调用 Win32 API，仅 Windows 可编译。
#[cfg(windows)]
mod job;
#[cfg(windows)]
mod sandbox;
#[cfg(windows)]
mod token;

use error::WboxError;
#[cfg(windows)]
use error::ErrKind;
#[cfg(windows)]
use job::JobLimits;

const USAGE: &str = r#"wbox — portable Windows 进程容器（AppContainer + Job Object）

用法:
  wbox run [OPTIONS] -- <CMD> [ARGS...]
  wbox image pull <REF> [--os linux] [--arch amd64] [--registry <HOST>] [-V]
  wbox image list
  wbox --help | -h
  wbox --version

选项:
  --name <NAME>     容器名（AppContainer profile 名），默认 "wbox-<pid>"
  --memory <MB>     每进程内存上限（MB），0 = 不限，默认 0
  --cpu-pct <N>     CPU 硬性百分比上限 1-100（Job CPU rate control），默认 0 = 不限
  --max-procs <N>   最大进程数，默认 0 = 不限
  --allow-network   授予 INTERNET_CLIENT capability（默认不授予任何网络能力）
  --no-network      显式声明不授予网络（默认行为，预留）
  --workdir <DIR>   容器工作目录（"镜像根"），默认当前目录
  --keep-profile    退出后保留 AppContainer profile（默认删除）
  --interactive     连接 stdio（当前默认且唯一支持的模式；--detach 预留）
  -V, --verbose     打印隔离配置摘要

示例:
  wbox run --memory 256 --cpu-pct 50 -- cmd.exe /c echo hello
  wbox run --name test1 --workdir C:\img\app --allow-network -- myapp.exe
"#;

/// `run` 子命令的全部参数。
#[cfg(windows)]
#[derive(Debug)]
struct RunOptions {
    name: Option<String>,
    limits: JobLimits,
    allow_network: bool,
    keep_profile: bool,
    workdir: Option<String>,
    verbose: bool,
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
            Err(WboxError::args("缺少子命令（run）"))
        }
    }
}

/// 手写参数解析：支持 `--opt value`、`--flag` 与 `--` 分隔的命令行。
#[cfg(windows)]
fn parse_run_args(args: &[String]) -> error::Result<RunOptions> {
    let mut opts = RunOptions {
        name: None,
        limits: JobLimits::default(),
        allow_network: false,
        keep_profile: false,
        workdir: None,
        verbose: false,
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
            "--workdir" => {
                opts.workdir = Some(take_value(args, &mut i, "--workdir")?);
            }
            "-V" | "--verbose" => opts.verbose = true,
            other if other.starts_with('-') => {
                return Err(WboxError::args(format!("未知选项 '{}'", other)));
            }
            // 容错：未写 "--" 时，第一个非选项参数起视为命令
            _ => {
                saw_dashdash = true;
                opts.cmd.push(a.clone());
            }
        }
        i += 1;
    }

    if opts.cmd.is_empty() {
        return Err(WboxError::args("缺少要执行的命令（-- <CMD> [ARGS...]）"));
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

/// 非 Windows 平台：run 不可用（隔离原语为 Win32 API）。
#[cfg(not(windows))]
fn cmd_run(_args: &[String]) -> error::Result<u32> {
    Err(WboxError::args(
        "run 子命令仅在 Windows 上可用（AppContainer/Job Object 为 Win32 原语）",
    ))
}

#[cfg(windows)]
fn cmd_run(args: &[String]) -> error::Result<u32> {
    let opts = parse_run_args(args)?;

    // 容器名：默认 wbox-<pid>
    let name = opts
        .name
        .unwrap_or_else(|| format!("wbox-{}", std::process::id()));

    // 工作目录：默认当前目录；必须是已存在的目录
    let workdir = match &opts.workdir {
        Some(d) => std::path::PathBuf::from(d),
        None => std::env::current_dir()
            .map_err(|e| WboxError::new(ErrKind::Args, anyhow::anyhow!(e).context("获取当前目录失败")))?,
    };
    // 只做存在性校验，不 canonicalize：std 的 canonicalize 会产生 `\\?\` 前缀
    // 的扩展路径，而 CreateProcessW 的 lpCurrentDirectory 不接受该前缀。
    if !workdir.is_dir() {
        return Err(WboxError::args(format!(
            "工作目录 '{}' 不存在或不是目录",
            workdir.display()
        )));
    }
    let workdir = workdir.to_string_lossy().into_owned();
    // 防御：若用户传入的 workdir 本身带 `\\?\` 前缀，去掉之。
    let workdir = workdir
        .strip_prefix(r"\\?\")
        .unwrap_or(&workdir)
        .to_string();

    // ---- 1. capability 集合（v1：仅 INTERNET_CLIENT）----
    let mut caps: Vec<token::CapabilitySid> = Vec::new();
    if opts.allow_network {
        caps.push(token::CapabilitySid::internet_client()?);
    }

    // ---- 2. AppContainer profile ----
    let mut profile = token::AppContainerProfile::create(&name, &caps)?;
    if opts.keep_profile {
        profile.keep();
    }

    // ---- 3. Job Object ----
    let job = job::Job::create(opts.limits)?;

    // ---- 4. verbose 摘要 ----
    if opts.verbose {
        println!("wbox 隔离配置:");
        println!("  profile 名   : {}", profile.name());
        println!("  AppContainer SID: {}", profile.sid_string()?);
        println!("  完整性级别   : Low（AppContainer 派生令牌内核强制）");
        println!(
            "  capabilities : {}",
            if caps.is_empty() {
                "（无，无网络能力）".to_string()
            } else {
                caps.iter().map(|c| c.desc()).collect::<Vec<_>>().join(", ")
            }
        );
        println!(
            "  Job 限额     : KILL_ON_JOB_CLOSE=on, memory={}MB, cpu={}%, max-procs={}",
            opts.limits.memory_mb, opts.limits.cpu_pct, opts.limits.max_procs
        );
        println!("  工作目录     : {}", workdir);
        println!("  keep-profile : {}", opts.keep_profile);
    }

    // ---- 5. 启动并等待 ----
    let cmdline = sandbox::build_cmdline(&opts.cmd)?;
    let code = sandbox::run_container(
        &profile,
        &caps,
        &cmdline,
        &workdir,
        &job,
    )?;

    if opts.verbose {
        println!("wbox: 子进程退出，退出码 = {}", code);
    }
    Ok(code)
}

/// `wbox image` 子命令：pull / list。
fn cmd_image(args: &[String]) -> error::Result<u32> {
    match args.first().map(|s| s.as_str()) {
        Some("pull") => cmd_image_pull(&args[1..]),
        Some("list") | Some("ls") => oci::list(),
        Some(other) => Err(WboxError::args(format!(
            "未知 image 子命令 '{}'（支持 pull / list）",
            other
        ))),
        None => Err(WboxError::args("image 缺少子命令（pull / list）")),
    }
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
