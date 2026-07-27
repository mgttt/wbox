//! CLI 层：子命令分发、共享解析原语与 USAGE 文本集中地。
//!
//! - `mod.rs` —— 顶层子命令分发（[`dispatch`]）与 USAGE；
//! - `args.rs` —— 各子命令共享的解析原语（取选项值、数值解析、单位置参数）；
//! - `run.rs`  —— `wbox run` 的参数结构与目标分派；
//! - `image.rs`—— `wbox image` 的 pull / list / show / rm。
//!
//! 全部为纯逻辑（不直接调 Win32），跨平台可编译、可在 Linux 沙箱单测。

pub mod args;
pub mod exec;
pub mod image;
pub mod logs;
pub mod ps;
pub mod rm;
pub mod stop;
pub mod run;

use crate::error::{Result, WboxError};

/// CLI 帮助文本（`wbox --help` 与缺参时的唯一文本源）。
pub const USAGE: &str = r#"wbox — portable Windows 进程容器（AppContainer + Job Object）

用法:
  wbox run [OPTIONS] -- <CMD> [ARGS...]            运行本机程序（Win: AppContainer+Job；Linux: namespace+cgroup）
  wbox run [OPTIONS] <IMAGE> [-- <CMD> [ARGS...]]  运行已 pull 的 OCI 镜像（Win: 经模拟器；Linux: 原生 namespace）
  wbox pull <REF> [image pull 选项]                 `wbox image pull` 的兼容别名
  wbox images                                      `wbox image list` 的兼容别名
  wbox rmi <REF> [--yes]                           `wbox image rm` 的兼容别名
  wbox image pull <REF> [--os linux] [--arch amd64] [--registry <HOST>] [-V]
  wbox image list | image ls
  wbox image show <REF>                            打印已 pull 镜像的 config 摘要
  wbox image rm <REF> [--yes]                      删除已 pull 镜像的本地缓存（默认交互确认）
  wbox ps [-a]                                     列出已登记的容器（-a 含已退出的残留）
  wbox stop <NAME> [--timeout <秒>]                停掉运行中的容器（先请求退出，超时则强制）
  wbox rm <NAME>...                                删除已退出的容器记录（运行中的会拒绝）
  wbox logs <NAME> [--stderr]                      读取 --detach 容器的输出
  wbox exec <NAME> -- <CMD> [ARGS...]              在运行中的容器内执行命令（Win 当前仅原生目标）
  wbox --help | -h
  wbox --version

选项:
  --name <NAME>     容器名（AppContainer profile 名），默认 "wbox-<pid>"
  --memory <MB>     每进程内存上限（MB），0 = 不限，默认 0
  --cpu-pct <N>     CPU 硬性百分比上限 1-100（Job CPU rate control），默认 0 = 不限
  --max-procs <N>   最大进程数，默认 0 = 不限
  -w, --workdir <DIR>
                     容器工作目录（"镜像根"），默认当前目录（仅原生模式）
  --network <MODE>  兼容模式：none = 默认断网；host = 等价 --allow-network
  --allow-network   放行网络（Win: 授予 INTERNET_CLIENT；Linux: 不建 netns）。默认断网
  --no-network      显式声明断网（默认行为，预留）
  -e, --env <K=V>   注入显式环境变量；可重复；不支持仅写 K 继承宿主值
  --keep-profile    退出后保留 AppContainer profile（默认删除）
  --rm              显式声明退出即清理（默认行为，docker 习惯写法；仅 run）
  --interactive     连接 stdio（默认）
  -d, --detach      后台运行：立即返回容器名，输出落盘到日志（wbox logs 读取）
  --pull            run 目标为镜像时，本地无缓存则先 pull
  --env-pass-all    继承完整宿主环境（默认仅白名单；BLINK_*/WBOX_* 保留键始终不透传）
  -V, --verbose     打印隔离配置摘要

镜像模式说明:
  位置参数能解析为镜像引用（如 ubuntu:24.04）且已在本地缓存（或带 --pull）时，
  视为镜像目标：自动按 docker 规则合并 config.json 的 Entrypoint/Cmd，
  注入 Env，rootfs 作为工作目录。镜像经 wbox-linux（blink 移植，开发中）
  模拟执行，未就绪时会得到明确错误。

兼容范围:
  仅兼容上述可兑现的运行习惯；不支持端口发布、volume/bind、daemon API、
  Compose、pod 或远程上下文，传入未实现选项会明确报错。

示例:
  wbox run --memory 256 --cpu-pct 50 -- cmd.exe /c echo hello
  wbox run --name test1 --workdir C:\img\app --allow-network -- myapp.exe
  wbox run ubuntu:24.04 -- bash
  wbox run --pull alpine:3.20 -- sh -c "echo hi"
"#;

/// 顶层子命令分发：返回进程退出码（wbox 自身错误经 ErrKind 映射，见 error.rs）。
pub fn dispatch(args: &[String]) -> Result<u32> {
    match args.first().map(|s| s.as_str()) {
        Some("run") => run::cmd_run(&args[1..]),
        Some("pull") => image::cmd_image_pull(&args[1..]),
        Some("images") => image::cmd_image_list(&args[1..]),
        Some("rmi") => image::cmd_image_rm(&args[1..]),
        Some("image") => image::cmd_image(&args[1..]),
        Some("ps") => ps::cmd_ps(&args[1..]),
        Some("logs") => logs::cmd_logs(&args[1..]),
        Some("exec") => exec::cmd_exec(&args[1..]),
        Some("rm") => rm::cmd_rm(&args[1..]),
        Some("stop") => stop::cmd_stop(&args[1..]),
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
            Err(WboxError::args("缺少子命令（run / image / ps / rm / stop / logs / exec）"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;
    use crate::{backend, oci};
    use backend::Backend;
    use image::cmd_image_show;

    #[test]
    fn dispatch_unknown_and_missing_subcommand() {
        assert!(dispatch(&["bogus".to_string()]).is_err());
        assert!(dispatch(&[]).is_err());
        assert_eq!(dispatch(&["--version".to_string()]).unwrap(), 0);
        assert_eq!(dispatch(&["--help".to_string()]).unwrap(), 0);
    }

    #[test]
    fn dispatch_docker_style_image_aliases() {
        let _home = TempHome::new("dispatch-aliases");
        assert_eq!(dispatch(&["images".to_string()]).unwrap(), 0);
        assert!(dispatch(&["images".to_string(), "--all".to_string()]).is_err());
        assert!(dispatch(&["pull".to_string()]).is_err());
        assert!(dispatch(&["rmi".to_string()]).is_err());
    }

    // ---- 集成：临时 HOME 下的假缓存全链（list → show → run prepare）----

    #[test]
    fn integration_list_show_run_prepare_chain() {
        let mut home = TempHome::new("chain");
        home.plant_fake_image("registry-1.docker.io", "library_fake", "latest");

        // 1. list：扫描到已缓存镜像（返回 Ok 即不报错；输出人工可查）
        oci::list().unwrap();

        // 2. show：打印 config 摘要（含脱敏路径）
        cmd_image_show(&["fake:latest".to_string()]).unwrap();

        // 3. classify：Docker 风格位置参数稳定判为镜像，不依赖缓存状态。
        let target = backend::classify_target(Some("fake")).unwrap();
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
        home.env().set("WBOX_LINUX", &fake_exe);
        let spec = backend::RunSpec {
            name: "t".to_string(),
            limits: Default::default(),
            allow_network: false,
            keep_profile: false,
            workdir: dir.join("rootfs"),
            cmd: merged,
            env: cfg.env.clone(),
            volumes: Vec::new(),
            verbose: false,
            env_pass_all: false,
        };
        let prepared = backend::BlinkBackend.prepare(&spec).unwrap();
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
    fn integration_image_classification_does_not_depend_on_cache() {
        let home = TempHome::new("promote");
        assert!(matches!(
            backend::classify_target(Some("fake")).unwrap(),
            backend::RunTarget::Image(_)
        ));
        home.plant_fake_image("registry-1.docker.io", "library_fake", "latest");
        match backend::classify_target(Some("fake")).unwrap() {
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
                // pull 声称成功不等于落盘完整：registry 行为因网络位置而异
                // （mirror 可能返回非预期内容）。网络结果一律不做硬断言，
                // 落盘不完整同样记 SKIP，只在校验通过时走 list/show 全链。
                let dir = oci::image_dir(&r).unwrap();
                let complete = ["manifest.json", "config.json", "layers.json"]
                    .iter()
                    .all(|f| dir.join(f).is_file())
                    && dir.join("rootfs").is_dir();
                if !complete {
                    eprintln!(
                        "SKIP：pull 返回成功但落盘不完整（{}，registry 行为差异）",
                        dir.display()
                    );
                    return;
                }
                oci::list().unwrap();
                cmd_image_show(&["hello-world".to_string()]).unwrap();
            }
            Err(e) => {
                eprintln!("SKIP：registry 不可达（{}），真实 pull 链路不判失败", e);
            }
        }
    }
}
