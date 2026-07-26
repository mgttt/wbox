//! `wbox run` 子命令：参数结构、手写解析与目标分派（原生 / 镜像）。

use crate::backend::{self, Backend, BlinkBackend, Limits, RunSpec, RunTarget};
use crate::error::{Result, WboxError};
use crate::oci;

/// `run` 子命令的全部参数（跨平台结构，纯逻辑可在 Linux 沙箱单测）。
#[derive(Debug)]
pub struct RunOptions {
    pub name: Option<String>,
    pub limits: Limits,
    pub allow_network: bool,
    pub keep_profile: bool,
    pub workdir: Option<String>,
    pub verbose: bool,
    /// 本地无缓存时先 pull（镜像模式）
    pub pull: bool,
    /// 继承完整宿主环境（默认仅白名单；保留键始终不透传）
    pub env_pass_all: bool,
    /// 第一个位置参数：镜像引用候选 或 本地命令首词
    pub positional: Option<String>,
    /// `--` 之后（或未写 `--` 时 positional 之后）的命令与参数
    pub cmd: Vec<String>,
}

/// 手写参数解析：支持 `--opt value`、`--flag`、至多一个位置参数（镜像引用
/// 候选 / 本地命令首词）与 `--` 分隔的命令行。
pub fn parse_run_args(args: &[String]) -> Result<RunOptions> {
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
                opts.name = Some(super::args::take_value(args, &mut i, "--name")?);
            }
            "--memory" => {
                let v = super::args::take_value(args, &mut i, "--memory")?;
                opts.limits.memory_mb = super::args::parse_u64("--memory", &v, "MB")?;
            }
            "--cpu-pct" => {
                let v = super::args::take_value(args, &mut i, "--cpu-pct")?;
                let n = super::args::parse_u32("--cpu-pct", &v)?;
                if n > 100 {
                    return Err(WboxError::args("--cpu-pct 取值范围为 0-100（0 = 不限）"));
                }
                opts.limits.cpu_pct = n;
            }
            "--max-procs" => {
                let v = super::args::take_value(args, &mut i, "--max-procs")?;
                opts.limits.max_procs = super::args::parse_u32("--max-procs", &v)?;
            }
            "--allow-network" => opts.allow_network = true,
            "--no-network" => opts.allow_network = false, // 显式默认，预留
            "--keep-profile" => opts.keep_profile = true,
            // docker 风格显式清理：v1 默认即为退出即删（profile RAII 删除 +
            // Job KILL_ON_JOB_CLOSE），接受并等价于默认（与 --no-network 同为显式默认）。
            "--rm" => opts.keep_profile = false,
            "--interactive" => {} // v1 唯一支持的模式，接受并忽略
            "--pull" => opts.pull = true,
            "--env-pass-all" => opts.env_pass_all = true,
            "--workdir" => {
                opts.workdir = Some(super::args::take_value(args, &mut i, "--workdir")?);
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

pub fn cmd_run(args: &[String]) -> Result<u32> {
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

/// 原生模式：本地 Windows 程序。prepare/参数组装跨平台（可在 Linux 单测）；
/// spawn 在非 Windows 平台给出明确错误（隔离原语为 Win32 API，见 native.rs）。
fn run_native(opts: &RunOptions, cmd: Vec<String>) -> Result<u32> {
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

/// 镜像模式：消费 config.json，经 BlinkBackend（wbox-linux 模拟）执行。
fn run_image(opts: &RunOptions, iref: oci::ImageRef) -> Result<u32> {
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


#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<RunOptions> {
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
    fn parse_rm_flag_accepted_as_explicit_default() {
        // --rm 等价默认：退出即清理（keep_profile=false）
        let o = parse(&["--rm", "--", "x"]).unwrap();
        assert!(!o.keep_profile);
        // 与 --keep-profile 同现后者覆盖（声明顺序生效）
        let o = parse(&["--rm", "--keep-profile", "--", "x"]).unwrap();
        assert!(o.keep_profile);
    }

    #[test]
    fn parse_interactive_flag_accepted() {
        let o = parse(&["--interactive", "--", "x"]).unwrap();
        assert_eq!(o.cmd, vec!["x"]);
    }

    // ---- image 子命令参数错误 ----
}
