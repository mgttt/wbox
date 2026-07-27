# wbox

`wbox` 是一个不依赖硬件虚拟化的 portable 进程容器。它在 Windows 上使用
AppContainer 和 Job Object 运行本机程序，也可以通过随包分发的
`wbox-linux.exe` 执行 Linux ELF 和 OCI 镜像；Linux 宿主使用 rootless
namespace、cgroup/rlimit，并可调用 Wine 运行 Windows CLI 程序。

项目面向 Windows 10/11/Server 等无法使用 WSL2 或 Hyper-V 的环境。默认模式
不要求管理员权限，不安装驱动，也不启用 Windows 可选功能。它提供进程级隔离，
不是 Docker 或虚拟机的等价替代品。

## 快速开始

```powershell
cargo build --release

# Windows 本机程序
.\target\release\wbox.exe run --memory 256 --cpu-pct 50 -- cmd.exe /c echo hello

# OCI 镜像
.\target\release\wbox.exe image pull ubuntu:24.04
.\target\release\wbox.exe run ubuntu:24.04 -- bash
```

Portable Windows 发布包包含两个文件：

- `wbox.exe`：CLI、OCI 客户端和宿主隔离后端。
- `wbox-linux.exe`：Windows 上执行 Linux x86-64 ELF 的运行时。

`wbox --help` 是 CLI 参数的唯一权威文本源。

## 能力边界

| 能力 | Windows 宿主 | Linux 宿主 |
|---|---|---|
| 运行宿主 CLI 程序 | AppContainer + Job Object | user/PID/mount namespace |
| 运行 Linux OCI 镜像 | `wbox-linux.exe` 用户态执行 | `pivot_root` 后原生执行 |
| 运行 Windows CLI 程序 | 原生执行 | 调用 Wine |
| CPU/内存/进程数限制 | Job Object | cgroup v2，受限时明确回退或拒绝 |
| 默认网络 | AppContainer 无网络 capability | 独立空 netns |
| 进程树清理 | Job kill-on-close | namespace/PDEATHSIG |

当前不提供文件系统 overlay、注册表虚拟化、端口映射、GUI 桌面隔离、驱动隔离，
也不提供 `ps/stop/exec/logs` 等后台容器生命周期管理。

## 文档

- [PRD.md](PRD.md)：功能树、需求、验收标准、进度、时间线和 agent 工作约定。
- [docs/architecture.md](docs/architecture.md)：后端、OCI、隔离和运行时技术参考。
- [docs/testing.md](docs/testing.md)：本地验证、测试分层和 CI 门禁。
- [vendor/blink/WIN32-PORT.md](vendor/blink/WIN32-PORT.md)：`wbox-linux` Win32
  移植手册。
- [tests/KNOWN-FAILURES.md](tests/KNOWN-FAILURES.md)：guest 回归问题台账。
- [CHANGELOG.md](CHANGELOG.md)：按版本记录的已交付变更。

开发任务先读 `PRD.md`。代码与文档冲突时，以代码、测试和 CI 配置为事实，
并在同一变更中修正文档。
