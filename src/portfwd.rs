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
//! network namespace 是**按线程**归属的，且父 user namespace 里 euid 相同的
//! 进程对子 namespace 持有权能。因此**多线程进程里的单个线程可以 `setns`
//! 进容器 netns**——实测 `setns` 返回 0 且该线程 `/proc/thread-self/ns/net`
//! 确实变成了容器的那个。
//!
//! 这一点决定了整个实现的形态：线程之间**共享 fd 表**，所以容器侧建立的
//! `TcpStream` 可以直接经 channel 交给宿主侧线程使用，**不需要 `SCM_RIGHTS`
//! 跨进程传 fd**，也不必每条连接 fork 一个助手。
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
    use std::net::{TcpListener, TcpStream};
    use std::os::unix::io::AsRawFd;
    use std::sync::mpsc;

    /// 向容器 netns 内发起连接的请求：附一个回程 channel。
    type ConnectRequest = mpsc::Sender<io::Result<TcpStream>>;

    /// 起一个常驻**容器 netns** 的连接器线程。
    ///
    /// 它是本模块的关键：`setns` 只改**调用线程**的 netns，所以这一个线程
    /// 进去之后就一直待在容器网络里，专职建立到 guest 端口的连接。建好的
    /// `TcpStream` 经 channel 回传——同进程线程共享 fd 表，宿主侧线程可以直接用。
    fn spawn_connector(container_pid: u32, guest_port: u16) -> Option<mpsc::Sender<ConnectRequest>> {
        let (tx, rx) = mpsc::channel::<ConnectRequest>();
        let ns_path = format!("/proc/{}/ns/net", container_pid);
        let f = std::fs::File::open(&ns_path).ok()?;
        let (ready_tx, ready_rx) = mpsc::channel::<bool>();
        std::thread::spawn(move || {
            // SAFETY: setns 只改本线程的 netns 归属，不触碰其它线程的状态。
            let ok = unsafe { libc::setns(f.as_raw_fd(), libc::CLONE_NEWNET) } == 0;
            let _ = ready_tx.send(ok);
            if !ok {
                return;
            }
            // 此后本线程的所有 socket 都建在容器网络里
            while let Ok(reply) = rx.recv() {
                let _ = reply.send(TcpStream::connect(("127.0.0.1", guest_port)));
            }
        });
        // 等连接器确认已进入容器 netns 再开始监听，否则首个连接会连到宿主上
        // ——那等于把宿主端口暴露成"容器端口"，比转发失败严重得多。
        match ready_rx.recv() {
            Ok(true) => Some(tx),
            _ => None,
        }
    }

    /// 双向对拷。任一方向结束就关掉写端，让对端读到 EOF 后自然收尾。
    fn relay(host: TcpStream, guest: TcpStream) {
        let (mut h_r, mut h_w) = (host.try_clone().ok(), host);
        let (mut g_r, mut g_w) = (guest.try_clone().ok(), guest);
        let (Some(hr), Some(gr)) = (h_r.take(), g_r.take()) else {
            return;
        };
        let up = std::thread::spawn(move || {
            let mut hr = hr;
            let _ = io::copy(&mut hr, &mut g_w);
            let _ = g_w.shutdown(std::net::Shutdown::Write);
        });
        let mut gr = gr;
        let _ = io::copy(&mut gr, &mut h_w);
        let _ = h_w.shutdown(std::net::Shutdown::Write);
        let _ = up.join();
    }

    /// 为一条映射起监听。绑定 **127.0.0.1** 而不是 0.0.0.0：
    /// 默认只对本机开放，避免一条 `-p` 就把容器端口暴露到局域网。
    pub fn serve(container_pid: u32, map: PortMap) -> std::io::Result<()> {
        let listener = TcpListener::bind(("127.0.0.1", map.host))?;
        let Some(connector) = spawn_connector(container_pid, map.guest) else {
            return Err(io::Error::other("无法进入容器 netns 建立连接器线程"));
        };
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(host_stream) = stream else { continue };
                let (rtx, rrx) = mpsc::channel();
                if connector.send(rtx).is_err() {
                    break; // 连接器已退出（容器没了）
                }
                match rrx.recv() {
                    Ok(Ok(guest_stream)) => {
                        std::thread::spawn(move || relay(host_stream, guest_stream));
                    }
                    // 容器内没人监听是常态（服务还没起来），不该刷屏
                    _ => continue,
                }
            }
        });
        Ok(())
    }
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
