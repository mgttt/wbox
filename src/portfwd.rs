//! `-p` 端口转发（`PRD.md` F9.2，仅 Linux 宿主）。
//!
//! # 为什么是用户态转发，而不是 veth 或 slirp
//!
//! - **veth/网桥**：建 veth 对要宿主 netns 里的 `CAP_NET_ADMIN`，rootless 下
//!   拿不到。
//! - **slirp4netns / pasta**：可行，但要用户先装——与"Portable：免安装"
//!   （PRD §2.2）直接冲突，只能当可选加速路径，不能作唯一实现。
//! - **本模块：wbox 自己转发，零外部依赖。**
//!
//! # 关键机制（实测确认，不是推断）
//!
//! 目标 netns 由容器自己的 user namespace 管辖。宿主进程不能只做
//! `setns(net)`：必须先进入 `user`，获得该 user namespace 内的 capability，
//! 再进入 `net`。Linux 又禁止多线程进程中的单个线程加入 user namespace，
//! 因此每条连接使用一个 wbox relay 子进程：`Command::pre_exec` 在 fork 后的
//! 单线程阶段按 `user -> net` 附着，已接受的宿主 socket 则作为 relay 的
//! stdin/stdout 传入。这样 listener 始终留在宿主 netns，guest 连接只会在
//! 容器 netns 建立。
//!
//! # 能力边界
//!
//! **只覆盖 TCP。** UDP 与 ICMP 这套做不了；文档必须写明，不能让用户以为
//! `-p` 等价于 Docker 的 `-p`。

use crate::error::{Result, WboxError};

/// 一条 `-p host:guest` 映射（TCP）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortMap {
    pub host: u16,
    pub guest: u16,
}

/// 解析 `-p` 取值：`HOST:GUEST` 或 `GUEST`（后者两端同号，与 docker 习惯一致）。
pub fn parse_port(spec: &str) -> Result<PortMap> {
    let bad = |why: &str| {
        WboxError::args(format!(
            "-p '{}' 无效：{}（用法 HOST:GUEST 或 PORT，仅 TCP）",
            spec, why
        ))
    };
    let port = |s: &str| -> Result<u16> {
        let n: u32 = s.parse().map_err(|_| bad("端口不是数字"))?;
        if n == 0 || n > 65535 {
            return Err(bad("端口必须在 1-65535"));
        }
        Ok(n as u16)
    };
    let parts: Vec<&str> = spec.split(':').collect();
    match parts.len() {
        1 => {
            let p = port(parts[0])?;
            Ok(PortMap { host: p, guest: p })
        }
        2 => Ok(PortMap {
            host: port(parts[0])?,
            guest: port(parts[1])?,
        }),
        _ => Err(bad("段数不对")),
    }
}

/// `-p` 与 `--allow-network` 同时给出时报错。
///
/// `--allow-network` 不建 netns、直接共享宿主网络栈，容器监听的端口**本来就在
/// 宿主上**，再做一层转发既无意义又会撞端口。静默忽略其中一个会让用户对实际
/// 生效的隔离strength产生错误认知，所以报错而不是二选一。
pub fn reject_conflicting_network(ports: &[PortMap], allow_network: bool) -> Result<()> {
    if ports.is_empty() || !allow_network {
        return Ok(());
    }
    Err(WboxError::args(
        "-p 与 --allow-network 不能同时使用：后者让容器直接共享宿主网络栈，\
         端口本就在宿主上，无需也不应再转发",
    ))
}

/// 端口转发在**当前宿主**是否可用。
pub fn reject_if_unsupported(ports: &[PortMap]) -> Result<()> {
    if ports.is_empty() || cfg!(target_os = "linux") {
        return Ok(());
    }
    Err(WboxError::args(
        "-p 端口转发目前只在 Linux 宿主可用（PRD §4.9 F9.2）",
    ))
}

#[cfg(target_os = "linux")]
mod imp {
    use super::PortMap;
    use std::io;
    use std::net::TcpListener;
    use std::os::fd::OwnedFd;
    use std::os::unix::process::CommandExt;
    use std::os::unix::io::AsRawFd;

    fn namespace_files(container_pid: u32) -> io::Result<Vec<(std::fs::File, libc::c_int)>> {
        [("user", libc::CLONE_NEWUSER), ("net", libc::CLONE_NEWNET)]
            .into_iter()
            .map(|(name, flag)| {
                std::fs::File::open(format!("/proc/{container_pid}/ns/{name}"))
                    .map(|file| (file, flag))
            })
            .collect()
    }

    fn spawn_relay(
        container_pid: u32,
        guest_port: u16,
        host_stream: std::net::TcpStream,
    ) -> io::Result<()> {
        let namespaces = namespace_files(container_pid)?;
        let input = host_stream.try_clone()?;
        let executable = std::env::current_exe()?;
        let mut command = std::process::Command::new(executable);
        command
            .arg("__port-relay")
            .arg(guest_port.to_string())
            .stdin(std::process::Stdio::from(OwnedFd::from(input)))
            .stdout(std::process::Stdio::from(OwnedFd::from(host_stream)));
        // SAFETY: pre_exec 在 fork 后、exec 前的单线程子进程里运行；这里只调用
        // async-signal-safe 的 setns，namespace fd 已在父进程预先打开。
        unsafe {
            command.pre_exec(move || {
                for (file, flag) in &namespaces {
                    if libc::setns(file.as_raw_fd(), *flag) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        // Child::drop 不会回收进程。每条活动连接单独等待，避免积累 zombie。
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    /// 为一条映射起监听。绑定 **127.0.0.1** 而不是 0.0.0.0：
    /// 默认只对本机开放，避免一条 `-p` 就把容器端口暴露到局域网。
    pub fn serve(container_pid: u32, map: PortMap) -> std::io::Result<()> {
        let listener = TcpListener::bind(("127.0.0.1", map.host))?;
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(host_stream) = stream else { continue };
                if let Err(error) = spawn_relay(container_pid, map.guest, host_stream) {
                    eprintln!(
                        "wbox: 端口 {}->{} 连接失败：{}",
                        map.host, map.guest, error
                    );
                }
            }
        });
        Ok(())
    }
}

/// relay 子进程入口。连接 guest 时短暂重试，消除 listener 已就绪而容器服务仍在
/// 启动的正常竞态；超过 5 秒仍不可达才让本次宿主连接收到 EOF。
#[cfg(target_os = "linux")]
pub fn cmd_internal_relay(args: &[String]) -> Result<u32> {
    use std::io::{self, Write};
    use std::net::{Shutdown, TcpStream};
    use std::time::{Duration, Instant};

    let port = match args {
        [port] => parse_port(port)?.host,
        _ => return Err(WboxError::args("__port-relay 需要一个 guest 端口")),
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut guest = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => break stream,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return Err(WboxError::spawn(format!(
                    "连接容器端口 {} 失败：{}",
                    port, error
                )))
            }
        }
    };
    let mut guest_writer = guest
        .try_clone()
        .map_err(|e| WboxError::spawn(format!("复制 guest socket 失败：{}", e)))?;
    let upload = std::thread::spawn(move || {
        let mut input = io::stdin();
        let _ = io::copy(&mut input, &mut guest_writer);
        let _ = guest_writer.shutdown(Shutdown::Write);
    });
    let mut output = io::stdout();
    io::copy(&mut guest, &mut output)
        .map_err(|e| WboxError::spawn(format!("读取容器端口 {} 失败：{}", port, e)))?;
    let _ = output.flush();
    let _ = upload.join();
    Ok(0)
}

/// 起转发。容器 pid 尚未记录时先等——它由 `runstate` 的记录线程异步写入。
#[cfg(target_os = "linux")]
pub fn spawn_forwarders(name: String, ports: Vec<PortMap>) {
    if ports.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let Ok(dir) = crate::runstate::dir_for(&name) else {
            return;
        };
        // 与 pid 记录线程赛跑：等它写出 container.pid
        let mut pid = None;
        for _ in 0..200 {
            if let Some(p) = crate::runstate::container_pid(&dir) {
                pid = Some(p);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let Some(pid) = pid else {
            eprintln!("wbox: 端口转发未启动——容器 pid 未记录");
            return;
        };
        for m in ports {
            if let Err(e) = imp::serve(pid, m) {
                eprintln!("wbox: 端口 {}->{} 转发启动失败：{}", m.host, m.guest, e);
            }
        }
    });
}

#[cfg(not(target_os = "linux"))]
pub fn spawn_forwarders(_name: String, _ports: Vec<PortMap>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_forms() {
        assert_eq!(parse_port("8080:80").unwrap(), PortMap { host: 8080, guest: 80 });
        // 单端口两端同号，与 docker 习惯一致
        assert_eq!(parse_port("8080").unwrap(), PortMap { host: 8080, guest: 8080 });
    }

    #[test]
    fn rejects_bad_ports() {
        for bad in ["0", "70000", "abc", "80:", ":80", "1:2:3", "8080:0"] {
            assert!(parse_port(bad).is_err(), "'{}' 应被拒绝", bad);
        }
    }

    /// `-p` 与 `--allow-network` 语义冲突，必须报错而不是静默二选一。
    #[test]
    fn port_conflicts_with_allow_network() {
        let p = [PortMap { host: 1, guest: 1 }];
        assert!(reject_conflicting_network(&p, false).is_ok());
        assert!(reject_conflicting_network(&[], true).is_ok(), "没给 -p 时不该报错");
        let e = reject_conflicting_network(&p, true).unwrap_err();
        assert!(format!("{}", e).contains("不能同时使用"), "{}", e);
    }

    /// 非 Linux 宿主要明确拒绝，不能静默忽略——用户会以为端口已经映射好了。
    #[test]
    fn unsupported_host_is_explicit() {
        let p = [PortMap { host: 1, guest: 1 }];
        let r = reject_if_unsupported(&p);
        if cfg!(target_os = "linux") {
            assert!(r.is_ok());
        } else {
            assert!(format!("{}", r.unwrap_err()).contains("只在 Linux"));
        }
    }
}
