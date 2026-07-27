//! `wbox run` 子命令：参数结构、手写解析与目标分派（原生 / 镜像）。

use crate::backend::{self, Backend, EmuBackend, Limits, RunSpec, RunTarget};

/// `ps` 里表示"非镜像目标"的占位串。
const NATIVE_TARGET: &str = "(native)";

/// 父进程用它告诉脱离出来的子进程"你是 supervisor，日志已备好、别再脱离一次"。
/// 同时也是 [`crate::runstate::register_with`] 保留日志的开关。
const SUPERVISED_ENV: &str = "WBOX_SUPERVISED";
const SUPERVISED_TOKEN_ENV: &str = "WBOX_SUPERVISED_TOKEN";
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
    /// `-e/--env KEY=VALUE` 显式注入；后出现的同名键覆盖镜像 Env。
    pub env: Vec<(String, String)>,
    /// 后台运行：立即返回，容器由脱离的 supervisor 进程持有（PRD F8.2）
    pub detach: bool,
    /// Docker/Podman `--rm`：后台容器退出后也删除状态记录与日志。
    pub auto_remove: bool,
    /// `-v host:guest[:ro]` 卷挂载（PRD F9.1）
    pub volumes: Vec<backend::VolumeMount>,
    /// `-p host:guest` 端口转发（PRD F9.2，仅 Linux）
    pub ports: Vec<crate::portfwd::PortMap>,
    /// `--restart` 重启策略（PRD F9.6）
    pub restart: crate::restart::RestartPolicy,
    /// `-u/--user UID[:GID]`（PRD F9.7，仅 Linux）
    pub user: Option<backend::UserSpec>,
    /// `--cap-drop` / `--cap-add`（PRD F9.8，仅 Linux）。先 drop 后 add。
    pub cap_drop: Vec<crate::caps::CapSelector>,
    pub cap_add: Vec<crate::caps::CapSelector>,
    /// `--seccomp-deny`（PRD F9.9，仅 Linux）。可重复，每项可逗号分隔。
    pub seccomp_deny: Vec<String>,
    /// `--health-*`（PRD F9.10，仅 Linux）。`None` = 没开健康检查。
    pub health: Option<crate::health::HealthSpec>,
    /// `--network container:<NAME>`（PRD F9.11，仅 Linux）：加入既有容器的
    /// netns，两容器经 localhost 互通。
    pub network_container: Option<String>,
    /// `--ipc container:<NAME>` / `--uts container:<NAME>`（PRD F9.15，仅 Linux）
    pub ipc_container: Option<String>,
    pub uts_container: Option<String>,
    /// `--hostname NAME`（仅 Linux；UTS namespace 已默认隔离）
    pub hostname: Option<String>,
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
        env: Vec::new(),
        detach: false,
        auto_remove: false,
        volumes: Vec::new(),
        ports: Vec::new(),
        restart: Default::default(),
        user: None,
        cap_drop: Vec::new(),
        cap_add: Vec::new(),
        seccomp_deny: Vec::new(),
        health: None,
        network_container: None,
        ipc_container: None,
        uts_container: None,
        hostname: None,
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
        // Docker/Podman 位置语义：首个位置参数是镜像；其后全部属于
        // guest command，不能再被 wbox 解析为 --name/-e/-w 等宿主选项。
        if opts.positional.is_some() {
            // 只把紧跟 IMAGE 的 `--` 当兼容分隔符；command 已开始后，
            // `sh -c SCRIPT -- ARG` 里的 `--` 是 guest 的真实 argv[0]。
            if a == "--" && opts.cmd.is_empty() {
                saw_dashdash = true;
            } else {
                opts.cmd.push(a.clone());
            }
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
            "--network" => {
                let mode = super::args::take_value(args, &mut i, "--network")?;
                if let Some(peer) = mode.strip_prefix("container:") {
                    if peer.is_empty() {
                        return Err(WboxError::args(
                            "--network container: 缺少容器名（用法 container:<NAME>）",
                        ));
                    }
                    opts.network_container = Some(peer.to_string());
                } else {
                    opts.allow_network = match mode.as_str() {
                        "none" => false,
                        "host" => true,
                        _ => {
                            return Err(WboxError::args(format!(
                                "--network 仅支持 none / host / container:<NAME>，得到 '{}'",
                                mode
                            )));
                        }
                    };
                }
            }
            // `--ipc`/`--uts` 目前只收 `container:<NAME>`：docker 还支持
            // `host`/`shareable`/`private`，但我们默认已是 private，而 `host`
            // 等于放弃刚补上的隔离——要开必须是显式且有理由的设计，不顺手加。
            "--ipc" | "--uts" => {
                let flag = a.clone();
                let v = super::args::take_value(args, &mut i, &flag)?;
                let peer = v.strip_prefix("container:").ok_or_else(|| {
                    WboxError::args(format!(
                        "{} 目前只支持 container:<NAME>（默认已是独立 namespace），得到 '{}'",
                        flag, v
                    ))
                })?;
                if peer.is_empty() {
                    return Err(WboxError::args(format!(
                        "{} container: 缺少容器名（用法 container:<NAME>）",
                        flag
                    )));
                }
                if flag == "--ipc" {
                    opts.ipc_container = Some(peer.to_string());
                } else {
                    opts.uts_container = Some(peer.to_string());
                }
            }
            "--hostname" | "-h" => {
                opts.hostname = Some(super::args::take_value(args, &mut i, "--hostname")?);
            }
            "--keep-profile" => opts.keep_profile = true,
            "--rm" => opts.auto_remove = true,
            "--interactive" => opts.detach = false, // 显式前台（默认）
            "--detach" | "-d" => opts.detach = true,
            "--restart" => {
                let v = super::args::take_value(args, &mut i, "--restart")?;
                opts.restart = crate::restart::parse_restart(&v)?;
            }
            "--health-cmd" => {
                let v = super::args::take_value(args, &mut i, "--health-cmd")?;
                opts.health = Some(crate::health::HealthSpec::new(v));
            }
            // 下面三个都只调参，不单独开启健康检查——没有 --health-cmd 就没有
            // 探针可跑，此时静默接受它们会让用户以为配好了。
            "--health-interval" | "--health-retries" | "--health-start-period" => {
                let flag = a.clone();
                let v = super::args::take_value(args, &mut i, &flag)?;
                let h = opts.health.as_mut().ok_or_else(|| {
                    WboxError::args(format!("{} 需要与 --health-cmd 同用", flag))
                })?;
                match flag.as_str() {
                    "--health-interval" => h.interval = crate::health::parse_secs(&flag, &v)?,
                    "--health-start-period" => {
                        h.start_period = crate::health::parse_secs(&flag, &v)?
                    }
                    _ => h.retries = super::args::parse_u32(&flag, &v)?,
                }
            }
            "--seccomp-deny" => {
                let v = super::args::take_value(args, &mut i, "--seccomp-deny")?;
                opts.seccomp_deny.push(v);
            }
            "--cap-drop" => {
                let v = super::args::take_value(args, &mut i, "--cap-drop")?;
                opts.cap_drop.push(crate::caps::parse_cap(&v)?);
            }
            "--cap-add" => {
                let v = super::args::take_value(args, &mut i, "--cap-add")?;
                opts.cap_add.push(crate::caps::parse_cap(&v)?);
            }
            "--user" | "-u" => {
                let v = super::args::take_value(args, &mut i, "--user")?;
                opts.user = Some(backend::parse_user(&v)?);
            }
            "--volume" | "-v" => {
                let v = super::args::take_value(args, &mut i, "--volume")?;
                opts.volumes.push(backend::parse_volume(&v)?);
            }
            "--pull" => opts.pull = true,
            "--env-pass-all" => opts.env_pass_all = true,
            "-e" | "--env" => {
                let raw = super::args::take_value(args, &mut i, a)?;
                let (key, value) = raw.split_once('=').ok_or_else(|| {
                    WboxError::args(format!(
                        "{} 仅支持 KEY=VALUE，不支持从宿主隐式继承：'{}'",
                        a, raw
                    ))
                })?;
                if key.is_empty() || key.contains('=') {
                    return Err(WboxError::args(format!("环境变量名非法：'{}'", key)));
                }
                if backend::env::is_reserved_key(key) {
                    return Err(WboxError::args(format!(
                        "环境变量 '{}' 为 wbox 隔离/凭证保留键，不能注入",
                        key
                    )));
                }
                opts.env.push((key.to_string(), value.to_string()));
            }
            "-w" | "--workdir" => {
                opts.workdir = Some(super::args::take_value(args, &mut i, a)?);
            }
            "-V" | "--verbose" => opts.verbose = true,
            // `-v/--volume` 已实现（见上）。这里留下的是**仍未实现**的几个：
            // 明确报错而不是静默忽略——静默忽略会让用户以为端口已经映射好了。
            // `--mount` 是另一套语法，即使 `-v` 能用也不代表它能用。
            "-p" | "--publish" => {
                let v = super::args::take_value(args, &mut i, "--publish")?;
                opts.ports.push(crate::portfwd::parse_port(&v)?);
            }
            // -P 是"发布镜像声明的所有端口"，需要读 config 的 ExposedPorts，
            // 与显式 -p 是两件事，尚未实现——明确报错而不是当成 -p 的别名。
            "-P" => {
                return Err(WboxError::args(
                    "选项 '-P' 不支持：自动发布镜像声明的全部端口尚未实现；请用 -p HOST:GUEST",
                ));
            }
            "--mount" => {
                return Err(WboxError::args(
                    "选项 '--mount' 不支持：请改用 -v host:guest[:ro]（PRD F9.1）",
                ));
            }
            other if other.starts_with('-') => {
                return Err(WboxError::args(format!("未知选项 '{}'", other)));
            }
            // 第一个非选项参数 = 镜像引用候选 / 本地命令首词。
            _ => opts.positional = Some(a.clone()),
        }
        i += 1;
    }

    // 在解析期就把 --name 拦下：容器名最终要交给
    // CreateAppContainerProfile，超长/空只会换回一个 E_INVALIDARG，
    // 与其他参数错误无法区分。此处提前给出可读错误（退出码 1 = 参数错误），
    // 且各平台的 cargo test 都能覆盖到这条规则。
    if let Some(name) = opts.name.as_deref() {
        crate::backend::validate_container_name(name)?;
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
        volumes: opts.volumes.clone(),
        ports: opts.ports.clone(),
        restart: opts.restart,
        user: opts.user,
        caps: crate::caps::CapPolicy::resolve(&opts.cap_drop, &opts.cap_add),
        // 解析已在 cmd_run 的前置校验里做过一次并报过错，这里不会失败；
        // 真失败也只会退化成"不装过滤器"，故用 unwrap_or_default 而非 expect。
        seccomp: crate::seccomp::SeccompPolicy::resolve(&opts.seccomp_deny).unwrap_or_default(),
        health: opts.health.clone(),
        network_container: opts.network_container.clone(),
        ipc_container: opts.ipc_container.clone(),
        uts_container: opts.uts_container.clone(),
        hostname: opts.hostname.clone(),
        direct_rootfs_writes: false,
        overlay_layer_dir: None,
        verbose: opts.verbose,
        env_pass_all: opts.env_pass_all,
    }
}

pub fn cmd_run(args: &[String]) -> Result<u32> {
    let opts = parse_run_args(args)?;
    // 这两条必须在 --detach 分叉**之前**：否则父进程照常返回容器名，真正的
    // 拒绝只会写进 supervisor 的日志里，用户还以为起成功了。
    validate_options(&opts)?;

    // --detach：把自己再拉起一份作为 supervisor，父进程立刻返回。
    // 容器的存活由 supervisor 持有（Job kill-on-close / PDEATHSIG 都绑在它身上），
    // 所以**不能**让父进程直接退出了事——那样容器会跟着一起没。
    if opts.detach && std::env::var_os(SUPERVISED_ENV).is_none() {
        return spawn_detached(&opts, args);
    }
    // supervisor 在任何 pull/copy/prepare 错误出口都要清掉父进程留下的预留；
    // 登记成功会先删除令牌，因此这个 guard 随后 drop 时自然成为 no-op。
    let _detached_guard = match std::env::var(SUPERVISED_TOKEN_ENV) {
        Ok(token) => {
            let name = opts
                .name
                .as_deref()
                .ok_or_else(|| WboxError::args("detached supervisor 缺少容器名"))?;
            Some(crate::runstate::adopt_detached(name, &token)?)
        }
        Err(_) => None,
    };

    // 可解析的镜像引用默认按镜像处理并在缺失时拉取；本地程序使用
    // `run -- PROGRAM` 或明确的路径/可执行文件名。
    let target = backend::classify_target(opts.positional.as_deref())?;

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

fn validate_options(opts: &RunOptions) -> Result<()> {
    crate::portfwd::reject_if_unsupported(&opts.ports)?;
    crate::restart::reject_conflicting_rm(opts.restart, opts.auto_remove)?;
    backend::reject_user_if_unsupported(opts.user)?;
    crate::caps::reject_if_unsupported(&crate::caps::CapPolicy::resolve(
        &opts.cap_drop,
        &opts.cap_add,
    ))?;
    // 解析错误（拼错的 syscall 名）必须在这里就报出来，而不是等到组装 RunSpec
    // 时被 unwrap_or_default 吞成"没配过"。
    let seccomp = crate::seccomp::SeccompPolicy::resolve(&opts.seccomp_deny)?;
    crate::seccomp::reject_if_unsupported(&seccomp)?;
    crate::seccomp::reject_unsupported_arch(&seccomp)?;
    crate::seccomp::reject_self_defeating(&seccomp)?;
    backend::reject_network_container_conflicts(
        opts.network_container.as_deref(),
        opts.allow_network,
        &opts.ports,
        opts.user,
    )?;
    crate::health::reject_if_unsupported(opts.health.as_ref())?;
    if let Some(h) = opts.health.as_ref() {
        crate::health::validate(h)?;
    }
    crate::portfwd::reject_conflicting_network(&opts.ports, opts.allow_network)
}

pub(crate) struct CreatedSummary {
    pub name: String,
    pub args: Vec<String>,
    pub cmd: Vec<String>,
    pub target: String,
    pub exec_context: crate::runstate::ExecContext,
}

/// 解析并验证 `create` 的运行配置，但不启动 workload 或创建 Windows 私有 rootfs。
pub(crate) fn prepare_create(args: &[String]) -> Result<CreatedSummary> {
    let opts = parse_run_args(args)?;
    if opts.detach {
        return Err(WboxError::args(
            "create 不接受 -d/--detach：容器会保持 created，执行 start 后才进入后台",
        ));
    }
    validate_options(&opts)?;
    let name = opts
        .name
        .clone()
        .unwrap_or_else(|| format!("wbox-{}", std::process::id()));
    let mut saved_args = args.to_vec();
    if opts.name.is_none() {
        saved_args.splice(0..0, ["--name".to_string(), name.clone()]);
    }
    let target = backend::classify_target(opts.positional.as_deref())?;

    match target {
        RunTarget::Native => {
            backend::reject_volumes_if_unsupported(&opts.volumes)?;
            let mut cmd = opts.positional.clone().into_iter().collect::<Vec<_>>();
            cmd.extend(opts.cmd.iter().cloned());
            backend::require_cmd(&cmd)?;
            let workdir = match &opts.workdir {
                Some(dir) => std::path::PathBuf::from(dir),
                None => std::env::current_dir()
                    .map_err(|e| WboxError::args(format!("获取当前目录失败：{}", e)))?,
            };
            if !workdir.is_dir() {
                return Err(WboxError::args(format!(
                    "工作目录 '{}' 不存在或不是目录",
                    workdir.display()
                )));
            }
            Ok(CreatedSummary {
                name,
                args: saved_args,
                cmd,
                target: NATIVE_TARGET.to_string(),
                exec_context: crate::runstate::ExecContext {
                    allow_network: opts.allow_network,
                    workdir: workdir.to_string_lossy().into_owned(),
                },
            })
        }
        RunTarget::Image(iref) => {
            backend::validate_image_volumes_for_host(&opts.volumes)?;
            let dir = ensure_image_cached(&opts, &iref)?;
            let config = oci::config::ImageConfig::load(&dir)?;
            let cmd = config
                .as_ref()
                .map(|value| value.merged_command(&opts.cmd))
                .unwrap_or_else(|| opts.cmd.clone());
            if cmd.is_empty() {
                return Err(WboxError::args(format!(
                    "镜像 '{}' 未声明 Entrypoint/Cmd，请显式给出命令",
                    iref.repo_tag()
                )));
            }
            Ok(CreatedSummary {
                name,
                args: saved_args,
                cmd,
                target: iref.qualified_ref(),
                exec_context: crate::runstate::ExecContext {
                    allow_network: opts.allow_network,
                    workdir: opts
                        .workdir
                        .clone()
                        .or_else(|| config.and_then(|value| value.working_dir))
                        .unwrap_or_else(|| "/".to_string()),
                },
            })
        }
    }
}

/// `--detach` 的父进程：建好状态目录与日志，脱离地拉起 supervisor，然后返回。
///
/// 日志文件由**父进程**创建并作为子进程的 stdio 传下去——子进程登记时因此要走
/// `keep_logs` 分支，否则常规的"复用残留"逻辑会把刚建好的日志删掉。
fn spawn_detached(opts: &RunOptions, args: &[String]) -> Result<u32> {
    let name = opts
        .name
        .clone()
        .unwrap_or_else(|| format!("wbox-{}", std::process::id()));
    let reservation = crate::runstate::reserve_detached(&name)?;
    let mut effective_args = args.to_vec();
    if opts.name.is_none() {
        effective_args.splice(0..0, ["--name".to_string(), name.clone()]);
    }
    spawn_reserved(name, &effective_args, reservation)
}

pub(crate) fn spawn_reserved(
    name: String,
    args: &[String],
    mut reservation: crate::runstate::DetachedReservation,
) -> Result<u32> {
    let dir = reservation.dir();
    let out = crate::runstate::open_log_append(dir, crate::runstate::LOG_STDOUT)?;
    let err = crate::runstate::open_log_append(dir, crate::runstate::LOG_STDERR)?;

    let exe = std::env::current_exe()
        .map_err(|e| WboxError::args(format!("定位 wbox 自身可执行文件失败：{}", e)))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("run");
    // 原样透传参数，只摘掉 --detach 自己（否则子进程会再脱离一次，无限套娃）
    for a in args {
        if a != "--detach" && a != "-d" {
            cmd.arg(a);
        }
    }
    cmd.env(SUPERVISED_ENV, "1")
        .env(SUPERVISED_TOKEN_ENV, reservation.token())
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err);
    detach_from_terminal(&mut cmd);

    let child = spawn_supervisor(&mut cmd)
        .map_err(|e| WboxError::spawn(format!("启动后台 wbox 失败：{}", e)))?;
    reservation.disarm();
    // 只打名字，方便脚本 `NAME=$(wbox run -d ...)` 直接取用
    println!("{}", name);
    // 不 wait：supervisor 要活得比我们久。它被 init 收养，不会成为僵尸。
    drop(child);
    Ok(0)
}

/// 让 supervisor 脱离当前终端/会话，免得终端一关就被 SIGHUP 带走。
#[cfg(target_os = "linux")]
fn detach_from_terminal(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: setsid 只改调用者自己的会话，且在 fork 与 exec 之间调用，
    // 此时进程是单线程的，不涉及 async-signal-unsafe 的操作。
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn detach_from_terminal(_cmd: &mut std::process::Command) {
    // Windows 侧的控制台与标准句柄继承由 `spawn_supervisor` 收口。
}

#[cfg(not(windows))]
fn spawn_supervisor(
    cmd: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    cmd.spawn()
}

#[cfg(windows)]
fn spawn_supervisor(
    cmd: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    let _guard = StandardHandleInheritanceGuard::new()?;
    cmd.spawn()
}

/// `Command` 会为显式配置的日志 stdio 准备可继承句柄，但 Windows 进程创建还可能
/// 顺带继承当前 wbox 的标准句柄。PowerShell 为 native-command pipeline 创建的
/// stdout/stderr 正属于后者；supervisor 长期持有它会让 `wbox start | Out-String`
/// 永远等不到 EOF。spawn 窗口内清掉这三个旧句柄的 inherit 位，随后原样恢复。
#[cfg(windows)]
struct StandardHandleInheritanceGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    handles: Vec<(windows_sys::Win32::Foundation::HANDLE, u32)>,
}

#[cfg(windows)]
impl StandardHandleInheritanceGuard {
    fn new() -> std::io::Result<Self> {
        use std::sync::{Mutex, OnceLock};
        use windows_sys::Win32::Foundation::{
            GetHandleInformation, SetHandleInformation, ERROR_INVALID_HANDLE,
            HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
        };
        use windows_sys::Win32::System::Console::{
            GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        };

        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let lock = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut guard = Self {
            _lock: lock,
            handles: Vec::new(),
        };

        for std_handle in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: std_handle 只取 Win32 定义的三个标准句柄常量。
            let handle = unsafe { GetStdHandle(std_handle) };
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                continue;
            }
            let mut flags = 0_u32;
            // SAFETY: handle 来自 GetStdHandle，flags 是有效输出地址。
            if unsafe { GetHandleInformation(handle, &mut flags) } == 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_INVALID_HANDLE as i32) {
                    continue;
                }
                return Err(error);
            }
            if flags & HANDLE_FLAG_INHERIT != 0 {
                // SAFETY: 只清除有效标准句柄的 inherit 位，Drop 会恢复原值。
                if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                guard.handles.push((handle, flags));
            }
        }
        Ok(guard)
    }
}

#[cfg(windows)]
impl Drop for StandardHandleInheritanceGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
        for (handle, flags) in self.handles.drain(..) {
            // SAFETY: 列表只含 new 中成功修改的句柄；析构期间尽力恢复 inherit 位。
            let _ = unsafe {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, flags & HANDLE_FLAG_INHERIT)
            };
        }
    }
}

/// supervisor 侧的日志限流：后台跑着的容器可能输出无止境，必须有界。
///
/// 用独立线程周期性检查体积、超限即丢旧留新（见 `runstate::enforce_log_cap`），
/// 而不是在 wbox 与 guest 之间插一层转写——后者要改动全部 Backend 的 stdio
/// 处理，代价远大于收益。
///
/// **诚实说明其边界**：这是周期采样，不是硬实时上限。两次 tick 之间容器可以
/// 短暂冲过上限，磁盘峰值取决于它 500ms 内能写多少。落盘的**最终**体积由
/// `spawn_with_state` 里退出后的那次收尾保证有界。要做到任意时刻严格不超，
/// 只能在 wbox 与 guest 之间插转写层，那是另一个量级的改动。
fn spawn_log_watchdog(dir: std::path::PathBuf) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        crate::runstate::enforce_log_cap(&dir, crate::runstate::LOG_STDOUT);
        crate::runstate::enforce_log_cap(&dir, crate::runstate::LOG_STDERR);
    });
}

/// 登记状态目录后再 spawn（PRD F8.1）。
///
/// 状态目录只在容器**运行期间**存在：`Registration` 是 RAII，spawn 返回即 drop
/// 并删目录。因此 `wbox ps` 看到的就是真在跑的东西，不需要额外的清理时机。
/// 崩溃时目录会残留，但锁随进程消失，`ps` 会如实标为 exited。
///
/// 收成一个函数而不是在四个 spawn 点各写一遍：漏掉任何一处都会让那条路径的
/// 容器在 `ps` 里隐身，而"少列了一个"比"多列了一个"难发现得多。
fn register_for_spawn(
    spec: &backend::RunSpec,
    target: &str,
    auto_remove: bool,
) -> Result<crate::runstate::Registration> {
    // 记 spec.cmd（用户要跑的 guest 命令）而不是 prepared.cmd：后者可能已被
    // 插入 wine/wbox-linux 前缀，对着 ps 看反而认不出自己起的是什么。
    let supervised = std::env::var_os(SUPERVISED_ENV).is_some();
    let exec_context = crate::runstate::ExecContext {
        allow_network: spec.allow_network,
        workdir: spec.workdir.to_string_lossy().into_owned(),
    };
    let reservation = std::env::var(SUPERVISED_TOKEN_ENV).ok();
    let mut reg = crate::runstate::register_with_context(
        &spec.name,
        &spec.cmd,
        target,
        supervised,
        Some(exec_context),
        reservation.as_deref(),
    )?;
    if supervised && auto_remove {
        reg.remove_on_drop();
    }
    Ok(reg)
}

/// 按 `--restart` 策略反复拉起 guest，返回**最后一次**的退出码。
///
/// 循环放在 supervisor 里而不是另起守护进程，除了不引入常驻服务
/// （PRD §2.2「免安装、无服务」），还白得一个正确性质：`wbox stop` 终止的正是
/// supervisor，**人为停掉的容器不会被自己重新拉起**——不需要额外维护一个
/// "这次是不是人为停的"标记，那种标记恰恰是容易与实际状态不同步的东西。
///
/// 代价对应地也说清：supervisor 崩溃时重启随之失效。要覆盖那种情况就得有
/// 常驻守护进程，与上面的前提冲突。
fn spawn_with_restart(
    b: &dyn Backend,
    spec: &backend::RunSpec,
    prepared: &backend::Prepared,
) -> Result<u32> {
    let mut restarts = 0u32;
    loop {
        // 每次拉起都刷新：restart 后 namespace owner 的宿主 PID 会变化，
        // exec/top/端口转发不能继续使用上一代的 container.pid。
        #[cfg(target_os = "linux")]
        {
            crate::runstate::clear_container_pid(&spec.name);
            crate::runstate::spawn_container_pid_recorder(spec.name.clone());
        }
        let rc = b.spawn(spec, prepared)?;
        if !spec.restart.should_restart(rc, restarts) {
            return Ok(rc);
        }
        restarts += 1;
        // 固定退避：一个起手就失败的容器若无间隔重启，会瞬间刷爆日志并空转
        // CPU。不做指数退避是因为容器重启的典型诉求是"尽快恢复服务"，
        // 退避太久反而违背意图；有上限的场景由 on-failure:N 控制。
        std::thread::sleep(std::time::Duration::from_millis(500));
        eprintln!(
            "wbox: 容器 '{}' 以退出码 {} 结束，按 --restart 策略第 {} 次重启",
            spec.name, rc, restarts
        );
    }
}

fn spawn_registered(
    b: &dyn Backend,
    spec: &backend::RunSpec,
    prepared: &backend::Prepared,
    reg: crate::runstate::Registration,
) -> Result<u32> {
    let supervised = std::env::var_os(SUPERVISED_ENV).is_some();
    if supervised {
        spawn_log_watchdog(crate::runstate::dir_for(&spec.name)?);
    }
    crate::portfwd::spawn_forwarders(spec.name.clone(), spec.ports.clone());
    #[cfg(target_os = "linux")]
    if let Some(h) = spec.health.clone() {
        crate::health::spawn_monitor(spec.name.clone(), h);
    }
    let rc = spawn_with_restart(b, spec, prepared);
    if supervised {
        if let Ok(code) = rc.as_ref() {
            crate::runstate::record_exit_code(reg.dir(), *code)?;
        }
        // 退出后再收一次尾。这条不是冗余：看门狗每 500ms 才看一眼，而一个
        // 一秒内狂写几百 MB 然后退出的容器**根本活不到第一次 tick**，只靠
        // 周期检查会让它整份输出原样落盘（实测 300k 行 2.5MB 全须全尾）。
        // 有了这一下，最终落盘体积无论容器活多久都是有界的。
        let dir = crate::runstate::dir_for(&spec.name)?;
        crate::runstate::enforce_log_cap(&dir, crate::runstate::LOG_STDOUT);
        crate::runstate::enforce_log_cap(&dir, crate::runstate::LOG_STDERR);
    }
    drop(reg);
    rc
}

fn spawn_with_state(
    b: &dyn Backend,
    spec: &backend::RunSpec,
    prepared: &backend::Prepared,
    target: &str,
    auto_remove: bool,
) -> Result<u32> {
    let reg = register_for_spawn(spec, target, auto_remove)?;
    spawn_registered(b, spec, prepared, reg)
}

/// 原生模式：**宿主自己的**程序。Windows 上是 AppContainer+Job（native.rs），
/// Linux 上是 namespace+cgroup 但**不换根**（linux.rs 的 `LinuxMode::Host`）
/// ——即 PRD.md F5 的宿主程序模式，供 harness 做环境控制。
/// 规则本体在 `backend::host_program_backend_kind`（可单测），此处只做分发。
fn run_native(opts: &RunOptions, cmd: Vec<String>) -> Result<u32> {
    let workdir = match &opts.workdir {
        Some(d) => std::path::PathBuf::from(d),
        None => std::env::current_dir()
            .map_err(|e| WboxError::args(format!("获取当前目录失败：{}", e)))?,
    };
    let spec = make_spec(opts, workdir, cmd, opts.env.clone());
    match backend::host_program_backend_kind() {
        backend::HostProgramBackendKind::LinuxNamespace => {
            let backend = backend::LinuxNativeBackend(backend::LinuxMode::Host);
            let prepared = backend.prepare(&spec)?;
            spawn_with_state(&backend, &spec, &prepared, NATIVE_TARGET, opts.auto_remove)
        }
        // Unsupported 也走 NativeBackend：它的 spawn 已经会给出明确的
        // "只能在 Windows 宿主上执行"错误，不必再重复一份文案。
        _ => {
            let backend = backend::NativeBackend;
            let prepared = backend.prepare(&spec)?;
            spawn_with_state(&backend, &spec, &prepared, NATIVE_TARGET, opts.auto_remove)
        }
    }
}

fn ensure_image_defaults(env: &mut Vec<(String, String)>) {
    if !env.iter().any(|(k, _)| k.eq_ignore_ascii_case("HOME")) {
        env.push(("HOME".to_string(), "/root".to_string()));
    }
}

fn ensure_image_cached(opts: &RunOptions, iref: &oci::ImageRef) -> Result<std::path::PathBuf> {
    let dir = oci::image_dir(iref)?;
    if !dir.join("rootfs").is_dir() {
        oci::pull(
            &iref.repo_tag(),
            "linux",
            "amd64",
            Some(&iref.registry),
            opts.verbose,
        )?;
        if !dir.join("rootfs").is_dir() {
            return Err(WboxError::registry(format!(
                "pull 后仍未找到镜像缓存 '{}'，无法运行",
                dir.display()
            )));
        }
    }
    Ok(dir)
}

/// 镜像模式：消费 config.json，经 EmuBackend（wbox-linux 模拟）执行。
fn run_image(opts: &RunOptions, iref: oci::ImageRef) -> Result<u32> {
    let dir = ensure_image_cached(opts, &iref)?;

    // 消费 image config：Env 注入、Entrypoint/Cmd 按 docker 规则合并、
    // WorkingDir 记录（guest 路径映射由 wbox-linux 侧落地，当前仅展示）。
    let img_cfg = oci::config::ImageConfig::load(&dir)?;
    let (merged, mut env) = match &img_cfg {
        Some(c) => (c.merged_command(&opts.cmd), c.env.clone()),
        None => (opts.cmd.clone(), Vec::new()),
    };
    env.extend(opts.env.iter().cloned());
    // Docker/Podman 会为默认 root 用户提供 HOME；不能继承宿主 HOME（会泄露
    // Windows 路径），也不能让 Fedora 的 dnf 等基础工具在无 HOME 时崩溃。
    ensure_image_defaults(&mut env);
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
    #[cfg(windows)]
    let mut spec = spec;
    // 按宿主分派（docs/architecture.md §3）：Windows 上 guest 是跑不了的
    // Linux ELF，必须经 wbox-linux 模拟；Linux 上宿主自己就能执行，走原生
    // namespace 隔离，省掉一整层模拟开销。规则本体在 backend::image_backend_kind
    // （可单测），此处只做分发。
    match backend::image_backend_kind() {
        backend::ImageBackendKind::Emu => {
            let backend = EmuBackend;
            #[cfg(windows)]
            let reg = {
                let reg = register_for_spawn(&spec, &iref.qualified_ref(), opts.auto_remove)?;
                let private_rootfs = reg.dir().join("rootfs");
                backend::create_private_rootfs(
                    &spec.workdir,
                    &private_rootfs,
                    &spec.name,
                )?;
                spec.workdir = private_rootfs;
                reg
            };
            let prepared = backend.prepare(&spec)?;
            #[cfg(windows)]
            {
                spawn_registered(&backend, &spec, &prepared, reg)
            }
            #[cfg(not(windows))]
            {
                spawn_with_state(
                    &backend,
                    &spec,
                    &prepared,
                    &iref.qualified_ref(),
                    opts.auto_remove,
                )
            }
        }
        backend::ImageBackendKind::LinuxNative => {
            let backend = backend::LinuxNativeBackend(backend::LinuxMode::Image);
            let prepared = backend.prepare(&spec)?;
            spawn_with_state(&backend, &spec, &prepared, &iref.qualified_ref(), opts.auto_remove)
        }
    }
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
    fn parse_preserves_all_guest_flags_after_image() {
        let o = parse(&[
            "ubuntu:24.04", "sh", "-c", "printf '%s' \"$1\"", "--name", "-e", "-w",
        ])
        .unwrap();
        assert_eq!(o.positional.as_deref(), Some("ubuntu:24.04"));
        assert_eq!(o.cmd, vec!["sh", "-c", "printf '%s' \"$1\"", "--name", "-e", "-w"]);
        assert!(o.name.is_none());
        assert!(o.env.is_empty());
        assert!(o.workdir.is_none());

        let o = parse(&["alpine", "sh", "-c", "echo \"$1\"", "--", "--name"]).unwrap();
        assert_eq!(o.cmd, vec!["sh", "-c", "echo \"$1\"", "--", "--name"]);
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

    #[test]
    fn parse_rejects_unimplemented_docker_capabilities() {
        for args in [
            // -v/--volume（F9.1）与 -p/--publish（F9.2）已实现，不再在此列。
            // 仍未实现的是：-P 要读镜像 config 的 ExposedPorts，与显式 -p 是两件事；
            // --mount 是另一套语法，-p/-v 能用不代表它能用。
            &["-P", "alpine"][..],
            &["--mount", "type=bind,src=/host,dst=/guest", "alpine"][..],
        ] {
            let err = parse(args).unwrap_err();
            assert!(format!("{}", err).contains("不支持"), "{:?}: {}", args, err);
        }
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
    fn parse_docker_workdir_and_network_aliases() {
        let o = parse(&["-w", "/app", "--network", "host", "--", "x"]).unwrap();
        assert_eq!(o.workdir.as_deref(), Some("/app"));
        assert!(o.allow_network);

        let o = parse(&["--allow-network", "--network", "none", "--", "x"]).unwrap();
        assert!(!o.allow_network);
        assert!(parse(&["--network", "bridge", "--", "x"]).is_err());
        assert!(parse(&["--network"]).is_err());
    }

    #[test]
    fn parse_explicit_env_compatibility_subset() {
        let o = parse(&[
            "-e", "FOO=one", "--env", "EMPTY=", "-e", "FOO=two", "--", "x",
        ])
        .unwrap();
        assert_eq!(
            o.env,
            vec![
                ("FOO".to_string(), "one".to_string()),
                ("EMPTY".to_string(), String::new()),
                ("FOO".to_string(), "two".to_string())
            ]
        );
        assert!(parse(&["-e", "HOST_ONLY", "--", "x"]).is_err());
        assert!(parse(&["--env", "=value", "--", "x"]).is_err());
        assert!(parse(&["-e", "WBOX_REGISTRY_PASS=secret", "--", "x"]).is_err());
    }

    #[test]
    fn cli_env_follows_image_env_for_override_precedence() {
        let opts = parse(&["-e", "PATH=/cli", "-e", "NEW=yes", "fake"]).unwrap();
        let mut env = vec![
            ("PATH".to_string(), "/image".to_string()),
            ("KEEP".to_string(), "ok".to_string()),
        ];
        env.extend(opts.env.iter().cloned());
        let built =
            backend::env::build_child_env(&env, &[], false, backend::env::GuestFlavor::Linux);
        assert!(built.iter().any(|(k, v)| k == "PATH" && v == "/cli"));
        assert!(built.iter().any(|(k, v)| k == "KEEP" && v == "ok"));
        assert!(built.iter().any(|(k, v)| k == "NEW" && v == "yes"));
    }

    #[test]
    fn image_home_default_does_not_override_explicit_value() {
        let mut env = Vec::<(String, String)>::new();
        ensure_image_defaults(&mut env);
        assert_eq!(env, vec![("HOME".to_string(), "/root".to_string())]);

        let mut env = vec![("HOME".to_string(), "/workspace".to_string())];
        ensure_image_defaults(&mut env);
        assert_eq!(env, vec![("HOME".to_string(), "/workspace".to_string())]);
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
    fn parse_rm_enables_lifecycle_auto_remove() {
        let o = parse(&["--rm", "--", "x"]).unwrap();
        assert!(!o.keep_profile);
        assert!(o.auto_remove);
        // profile 生命周期与容器记录生命周期相互独立。
        let o = parse(&["--rm", "--keep-profile", "--", "x"]).unwrap();
        assert!(o.keep_profile);
        assert!(o.auto_remove);
    }

    #[test]
    fn parse_interactive_flag_accepted() {
        let o = parse(&["--interactive", "--", "x"]).unwrap();
        assert_eq!(o.cmd, vec!["x"]);
    }

    // ---- image 子命令参数错误 ----

    // ---- --name 在解析期即校验（规则见 backend::validate_container_name）----

    #[test]
    fn parse_rejects_overlong_name_at_parse_time() {
        let long = "a".repeat(65);
        let err = parse(&["--name", &long, "--", "cmd.exe"]).unwrap_err();
        assert_eq!(err.exit_code(), 1, "应归类为参数错误：{}", err);
        assert!(format!("{}", err).contains("容器名"), "{}", err);
    }

    #[test]
    fn parse_rejects_empty_name() {
        assert!(parse(&["--name", "", "--", "cmd.exe"]).is_err());
    }

    #[test]
    fn parse_accepts_boundary_length_name() {
        let ok = "a".repeat(64);
        assert!(parse(&["--name", &ok, "--", "cmd.exe"]).is_ok());
    }

    #[test]
    fn default_name_is_within_limit() {
        // 缺省名 wbox-<pid> 必须天然合法，否则不带 --name 就跑不起来
        let opts = parse(&["--", "cmd.exe"]).unwrap();
        assert!(opts.name.is_none());
        let default = format!("wbox-{}", std::process::id());
        assert!(crate::backend::validate_container_name(&default).is_ok(), "{}", default);
    }
}
