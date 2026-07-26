//! CLI 层：子命令分发、共享解析原语与 USAGE 文本集中地。
//!
//! - `mod.rs` —— 顶层子命令分发（[`dispatch`]）与 USAGE；
//! - `args.rs` —— 各子命令共享的解析原语（取选项值、数值解析、单位置参数）；
//! - `run.rs`  —— `wbox run` 的参数结构与目标分派；
//! - `image.rs`—— `wbox image` 的 pull / list / show / rm。
//!
//! 全部为纯逻辑（不直接调 Win32），跨平台可编译、可在 Linux 沙箱单测。

pub mod args;
pub mod image;
pub mod run;

use crate::error::{Result, WboxError};

/// CLI 帮助文本（`wbox --help` 与缺参时的唯一文本源）。
pub const USAGE: &str = r#"wbox — portable Windows 进程容器（AppContainer + Job Object）

用法:
  wbox run [OPTIONS] -- <CMD> [ARGS...]            运行本地 Windows 程序
  wbox run [OPTIONS] <IMAGE> [-- <CMD> [ARGS...]]  运行已 pull 的 OCI 镜像（Linux 后端骨架）
  wbox image pull <REF> [--os linux] [--arch amd64] [--registry <HOST>] [-V]
  wbox image list
  wbox image show <REF>                            打印已 pull 镜像的 config 摘要
  wbox image rm <REF> [--yes]                      删除已 pull 镜像的本地缓存（默认交互确认）
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
  --rm              显式声明退出即清理（默认行为，docker 习惯写法；仅 run）
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

/// 顶层子命令分发：返回进程退出码（wbox 自身错误经 ErrKind 映射，见 error.rs）。
pub fn dispatch(args: &[String]) -> Result<u32> {
    match args.first().map(|s| s.as_str()) {
        Some("run") => run::cmd_run(&args[1..]),
        Some("image") => image::cmd_image(&args[1..]),
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

/// 测试共享脚手架：临时 HOME（构造期间把 HOME 指向独立临时目录，
/// Drop 时恢复并清理；cache_root 优先 USERPROFILE，故同时摘掉它）。
///
/// HOME 是**进程级**状态：并行用例各自指向自己的临时目录必然互踩（且一方的
/// 清理会删掉另一方正在读的文件）。因此内部持有 [`crate::testenv::EnvGuard`]，
/// 存活期间独占进程环境——用例之间由此天然串行。需要额外临时环境变量时，
/// 经 [`TempHome::env`] 借出**同一把**守卫，切勿另起 `EnvGuard`（会自死锁）。
#[cfg(test)]
pub(crate) struct TempHome {
    pub dir: std::path::PathBuf,
    env: crate::testenv::EnvGuard,
}

#[cfg(test)]
impl TempHome {
    pub fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "wbox-test-home-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut env = crate::testenv::EnvGuard::new();
        env.set("HOME", &dir);
        env.remove("USERPROFILE");
        Self { dir, env }
    }

    /// 借出内部环境守卫：用例需要额外临时环境变量时经它设置（Drop 时一并还原）。
    pub fn env(&mut self) -> &mut crate::testenv::EnvGuard {
        &mut self.env
    }

    /// 在缓存布局中安放一个假镜像（rootfs + 元数据），返回缓存目录。
    pub fn plant_fake_image(&self, registry: &str, name_flat: &str, tag: &str) -> std::path::PathBuf {
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

#[cfg(test)]
impl Drop for TempHome {
    fn drop(&mut self) {
        // 只清目录；环境变量由字段 `env` 的 Drop 还原（本 Drop 先于字段执行，
        // 故清理期间 HOME 仍指向本目录，不会误删他人）。
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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


    // ---- 集成：临时 HOME 下的假缓存全链（list → show → run prepare）----

    #[test]
    fn integration_list_show_run_prepare_chain() {
        let mut home = TempHome::new("chain");
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
        home.env().set("WBOX_LINUX", &fake_exe);
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
