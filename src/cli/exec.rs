//! `wbox exec`：在**已经在跑**的容器里再执行一条命令（`PRD.md` §4.9 L1 / F8.4）。
//!
//! harness 场景要的"起一个容器，再往里发若干命令"就靠它；没有它，每条命令都得
//! 重起一个容器，环境既不复用也不连续。
//!
//! # 附着顺序（实测确认，别改动顺序）
//!
//! 附着到状态目录记的 `container.pid`（**容器内**那个进程；`meta.json` 里的
//! `pid` 是 supervisor，留在宿主 namespace，附错了等于没隔离）：
//!
//! 1. `setns(user)` **必须最先**——进了目标 user namespace 才拿得到其中的特权，
//!    顺序反了后面几个会因权限不足失败。
//! 2. `setns(mnt)`、`setns(net)`。
//! 3. PID 用 `/proc/<pid>/ns/pid_for_children` 而**不是** `ns/pid`：
//!    `unshare(CLONE_NEWPID)` 对调用者自己不生效、只对其之后的子进程生效，
//!    所以那个进程的 `ns/pid` 仍指向宿主。实测印证：同一进程
//!    `pid -> pid:[4026531836]`（宿主）而 `pid_for_children -> pid:[4026532296]`
//!    （容器）。
//! 4. 附着 PID namespace 后**必须再 fork 一次**。`setns(CLONE_NEWPID)` 与
//!    `unshare` 同理：**对调用者自己不生效，只对其之后创建的子进程生效**。
//!    `pre_exec` 已经在 `Command::spawn` 的 fork **之后**运行，所以指望它
//!    "天然落在容器视图里"是错的——实测过：只 setns 不再 fork 时，容器内
//!    `echo $$` 打出的是宿主 pid（19746 这种大数），而不是容器视图里的小号。
//!    因此闭包里自己再 fork：父进程等子进程并用同样的码退出，子进程才 exec。
//!
//! # Windows
//!
//! Windows 没有 namespace 的“进入”原语。原生目标采用可对齐子集：重新派生
//! 同一 AppContainer SID、按原网络策略重建 capability，并把挂起的新进程加入
//! 同一命名 Job。OCI/Blink 缺少可靠的 rootfs/env 重建语义，明确拒绝。

use crate::error::{Result, WboxError};
use crate::runstate::{self, Liveness};

struct ExecOptions<'a> {
    name: &'a str,
    cmd: Vec<&'a str>,
}

fn parse(args: &[String]) -> Result<ExecOptions<'_>> {
    let mut name: Option<&str> = None;
    let mut cmd: Vec<&str> = Vec::new();
    let mut saw_dashdash = false;
    for a in args {
        if saw_dashdash || !cmd.is_empty() {
            cmd.push(a);
            continue;
        }
        match a.as_str() {
            "--" => saw_dashdash = true,
            other if other.starts_with('-') => {
                return Err(WboxError::args(format!(
                    "exec: 未知参数 '{}'（用法：wbox exec <NAME> -- <CMD> [ARGS...]）",
                    other
                )))
            }
            other => {
                if name.is_some() {
                    // COMMAND 一旦开始，后续内容全部原样透传，包括 `-` 开头
                    // 的 guest 参数；与 docker/podman exec 的位置语义一致。
                    cmd.push(other);
                } else {
                    name = Some(other);
                }
            }
        }
    }
    let name = name
        .ok_or_else(|| WboxError::args("exec: 缺少容器名（用法：wbox exec <NAME> -- <CMD>）"))?;
    if cmd.is_empty() {
        return Err(WboxError::args(
            "exec: 缺少要执行的命令（wbox exec <NAME> -- <CMD> [ARGS...]）",
        ));
    }
    Ok(ExecOptions { name, cmd })
}

pub fn cmd_exec(args: &[String]) -> Result<u32> {
    let opts = parse(args)?;
    exec_existing(opts.name, &opts.cmd)
}

#[cfg(target_os = "linux")]
fn exec_existing(name: &str, cmd: &[&str]) -> Result<u32> {
    let dir = runstate::resolve_existing(name)?;
    // 已退出的容器没有可附着的 namespace。必须明确拒绝——否则命令会跑在**宿主**
    // 上，而用户以为它跑在容器里，这比报错危险得多。
    if runstate::liveness(&dir) == Liveness::Exited {
        return Err(WboxError::args(format!(
            "容器 '{}' 已退出，无法 exec（namespace 已随之消失）",
            name
        )));
    }
    let pid = runstate::container_pid(&dir).ok_or_else(|| {
        WboxError::args(format!(
            "容器 '{}' 未记录容器内 pid，无法附着（容器可能刚启动，稍后重试）",
            name
        ))
    })?;
    exec_in_namespaces(pid, cmd)
}

#[cfg(windows)]
fn exec_existing(name: &str, cmd: &[&str]) -> Result<u32> {
    let locked = runstate::lock_existing(name)?;
    if runstate::liveness(&locked.dir) == Liveness::Exited {
        return Err(WboxError::args(format!(
            "容器 '{}' 已退出，无法 exec",
            name
        )));
    }
    if locked.entry.stopping {
        return Err(WboxError::args(format!(
            "容器 '{}' 正在停止，无法 exec",
            name
        )));
    }
    if locked.entry.target != "(native)" {
        return Err(WboxError::args(format!(
            "Windows exec 当前只支持原生容器；'{}' 的目标是 '{}'，\
             OCI/Blink 无法可靠重建 rootfs 与环境",
            name, locked.entry.target
        )));
    }
    let context = locked.entry.exec_context.clone().ok_or_else(|| {
        WboxError::args(format!(
            "容器 '{}' 缺少 Windows exec 上下文（可能由旧版 wbox 启动），请重建容器",
            name
        ))
    })?;
    let cmd: Vec<String> = cmd.iter().map(|arg| (*arg).to_string()).collect();
    let workdir = std::path::PathBuf::from(&context.workdir);
    crate::backend::NativeBackend::exec_existing(
        name,
        context.allow_network,
        &workdir,
        &cmd,
        move || drop(locked),
    )
}

#[cfg(not(any(target_os = "linux", windows)))]
fn exec_existing(_name: &str, _cmd: &[&str]) -> Result<u32> {
    Err(WboxError::spawn(
        "exec 目前只支持 Linux 容器和 Windows 原生容器",
    ))
}

#[cfg(target_os = "linux")]
fn exec_in_namespaces(pid: u32, cmd: &[&str]) -> Result<u32> {
    use std::os::unix::io::AsRawFd;
    use std::os::unix::process::CommandExt;

    // 顺序见模块文档：user 最先，PID 用 pid_for_children。
    let wanted: [(&str, libc::c_int); 4] = [
        ("user", libc::CLONE_NEWUSER),
        ("mnt", libc::CLONE_NEWNS),
        ("net", libc::CLONE_NEWNET),
        ("pid_for_children", libc::CLONE_NEWPID),
    ];
    let mut fds: Vec<(std::fs::File, libc::c_int)> = Vec::new();
    for (ns, flag) in wanted {
        let path = format!("/proc/{}/ns/{}", pid, ns);
        match std::fs::File::open(&path) {
            Ok(f) => fds.push((f, flag)),
            // --allow-network 时不建 netns，缺 net 是正常的；其余缺失才是问题
            Err(_) if ns == "net" => continue,
            Err(e) => {
                return Err(WboxError::spawn(format!(
                    "打开容器 namespace '{}' 失败：{}（容器可能刚退出）",
                    path, e
                )))
            }
        }
    }

    let mut c = std::process::Command::new(cmd[0]);
    c.args(&cmd[1..]);
    // SAFETY: 闭包运行在 fork 之后、exec 之前的单线程子进程里，只调用
    // async-signal-safe 的 setns；fd 由闭包按值捕获，内部不做任何分配。
    unsafe {
        c.pre_exec(move || {
            for (f, flag) in &fds {
                if libc::setns(f.as_raw_fd(), *flag) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            // 再 fork 一次，孙进程才真正在容器的 PID 视图里（见模块文档第 4 条）。
            // 结构与 linux_ns::enter_namespaces 的双 fork 一致。
            let pid = libc::fork();
            if pid < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if pid > 0 {
                // 中间进程：等孙进程结束并用同样的码退出。这里绝不能 return Ok
                // ——否则它会继续 exec，变成两份用户命令。
                let mut st: libc::c_int = 0;
                while libc::waitpid(pid, &mut st, 0) < 0 {
                    if *libc::__errno_location() != libc::EINTR {
                        libc::_exit(127);
                    }
                }
                // WIFEXITED / WEXITSTATUS 的等价位运算（libc crate 未导出宏）
                let code = if st & 0x7f == 0 { (st >> 8) & 0xff } else { 128 + (st & 0x7f) };
                libc::_exit(code);
            }
            Ok(())
        });
    }
    let mut child = c
        .spawn()
        .map_err(|e| WboxError::spawn(format!("在容器内启动 '{}' 失败：{}", cmd[0], e)))?;
    let status = child
        .wait()
        .map_err(|e| WboxError::spawn(format!("等待 exec 进程失败：{}", e)))?;
    // 退出码语义与 run 一致：原样转发，信号终止用 128+sig
    Ok(match status.code() {
        Some(c) => c as u32,
        None => {
            use std::os::unix::process::ExitStatusExt;
            128 + status.signal().unwrap_or(0) as u32
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn parse_splits_name_and_command() {
        let a = [
            "c1".to_string(),
            "--".to_string(),
            "ls".to_string(),
            "-l".to_string(),
        ];
        let o = parse(&a).unwrap();
        assert_eq!(o.name, "c1");
        assert_eq!(o.cmd, vec!["ls", "-l"]);
        let b = ["c1".to_string(), "ls".to_string()];
        assert_eq!(parse(&b).unwrap().cmd, vec!["ls"]);
        let c = [
            "c1".to_string(),
            "powershell.exe".to_string(),
            "-NoProfile".to_string(),
            "-Command".to_string(),
            "echo ok".to_string(),
        ];
        assert_eq!(
            parse(&c).unwrap().cmd,
            vec!["powershell.exe", "-NoProfile", "-Command", "echo ok"]
        );
        assert!(parse(&[]).is_err(), "缺名字应报错");
        assert!(parse(&["c1".to_string()]).is_err(), "缺命令应报错");
        assert!(parse(&["--bogus".to_string()]).is_err());
    }

    /// 已退出的容器必须明确拒绝——否则命令会在**宿主**上跑，
    /// 而用户以为它跑在容器里。
    #[test]
    fn exited_container_is_rejected() {
        let _home = TempHome::new("exited");
        let dir = runstate::dir_for("dead").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lock"), b"").unwrap();
        std::fs::write(
            dir.join("meta.json"),
            r#"{"name":"dead","pid":1,"created_unix":1,"cmd":["x"],"target":"(native)"}"#,
        )
        .unwrap();
        let err =
            cmd_exec(&["dead".to_string(), "--".to_string(), "true".to_string()]).unwrap_err();
        assert!(format!("{}", err).contains("已退出"), "{}", err);
    }

    /// 运行中但还没记下 container.pid（容器刚起）要给出可懂的提示。
    #[cfg(target_os = "linux")]
    #[test]
    fn running_without_recorded_pid_explains() {
        let _home = TempHome::new("nopid");
        let _reg = runstate::register("live", &["/bin/true".into()], "(native)").unwrap();
        let err =
            cmd_exec(&["live".to_string(), "--".to_string(), "true".to_string()]).unwrap_err();
        assert!(format!("{}", err).contains("未记录容器内 pid"), "{}", err);
    }

    #[cfg(windows)]
    #[test]
    fn windows_image_exec_is_rejected_before_spawn() {
        let _home = TempHome::new("windows-image");
        let _reg = runstate::register_with_context(
            "image",
            &["sleep".into()],
            "alpine:3.20",
            false,
            Some(runstate::ExecContext {
                allow_network: false,
                workdir: std::env::temp_dir().to_string_lossy().into_owned(),
            }),
        )
        .unwrap();
        let err =
            cmd_exec(&["image".to_string(), "--".to_string(), "cmd.exe".to_string()]).unwrap_err();
        assert!(format!("{}", err).contains("只支持原生容器"), "{}", err);
    }

    #[test]
    fn unknown_container_is_an_error() {
        let _home = TempHome::new("unknown");
        assert!(cmd_exec(&["nope".to_string(), "--".to_string(), "true".to_string()]).is_err());
    }

    #[test]
    fn name_cannot_escape_run_root() {
        let _home = TempHome::new("escape");
        assert!(cmd_exec(&["../evil".to_string(), "--".to_string(), "true".to_string()]).is_err());
    }
}
