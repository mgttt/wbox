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

# 后台运行
.\target\release\wbox.exe run -d --name job1 ubuntu:24.04 -- bash -c "echo hi"
.\target\release\wbox.exe ps -a
.\target\release\wbox.exe logs job1
.\target\release\wbox.exe rm job1
```

Portable Windows 发布包包含两个文件：

- `wbox.exe`：CLI、OCI 客户端和宿主隔离后端。
- `wbox-linux.exe`：Windows 上执行 Linux x86-64 ELF 的运行时。

`wbox --help` 是 CLI 参数的唯一权威文本源。

## 对标基线

| 象限 | 参照物 | 主要差距 |
|---|---|---|
| Windows × Windows 程序 | Sandboxie-Plus | **无文件系统写重定向、无注册表虚拟化**（撞驱动天花板）|
| Windows × Linux 镜像 | WSL2 / Docker Desktop | 已有 `build` 子集+分层缓存/`--restart`；无卷挂载/端口映射/`--user`，**性能不可比** |
| Linux × Linux 镜像 | Podman / Docker | 已有 `-v`/`-p`（仅 TCP）/`build`+分层缓存/`--restart`/`--user`（数字 id）/`--cap-add`/`--cap-drop`/`--seccomp-deny`（拒绝名单）/healthcheck/`--network container:`/overlay 可写层/`push`（平铺单层）；无 compose、自定义 bridge 网络、镜像分层存储 |
| Linux × Windows 程序 | Wine | 依赖宿主已装 Wine；GUI 未覆盖（wineprefix 已按容器隔离） |

**两条硬天花板**（不说破的话"对标"只是口号）：

1. wbox **不装内核驱动**——这是"免安装、不要管理员权限"的直接代价。因此
   Windows 程序沙箱达不到 Sandboxie（minifilter）级别的写重定向完整性。
2. 没有虚拟化时，Windows 上跑 Linux 镜像靠用户态执行，**性能与 WSL2 不是一个
   量级**；定位是"没有 VT-x/WSL2 时仍然能跑"。

逐条能力对照见 [PRD.md](PRD.md) §2.4，天花板见 §2.5。

## 能力边界

| 能力 | Windows 宿主 | Linux 宿主 |
|---|---|---|
| 运行宿主 CLI 程序 | AppContainer + Job Object | user/PID/mount namespace |
| 运行 Linux OCI 镜像 | `wbox-linux.exe` 用户态执行 | `pivot_root` 后原生执行 |
| 运行 Windows CLI 程序 | 原生执行 | 调用 Wine |
| CPU/内存/进程数限制 | Job Object | cgroup v2，受限时明确回退或拒绝 |
| 默认网络 | AppContainer 无网络 capability | 独立空 netns |
| 进程树清理 | Job kill-on-close | namespace/PDEATHSIG |
| 后台运行与生命周期 | `--detach` / `ps` / `logs` / `stop` / `rm` | 同左，另有 `exec` |

后台容器：`--detach` 起，`ps` 看，`logs` 读输出（容器跑完仍可读），
`stop` 停整棵进程树，`rm` 清记录。日志体积有上限，截断处会写明。
`stop` 在 Linux 上先 `SIGTERM` 再 `SIGKILL`；Windows 无 `SIGTERM` 等价物，
直接强制终止。

两侧代码同一套，Windows 侧的存活判定与进程终止有单测在 windows runner 上真跑；
但**端到端的后台流程目前只在 Linux 有门禁**，Windows 上尚未逐条验证过。

当前不提供文件系统 overlay、注册表虚拟化、GUI 桌面隔离、驱动隔离；端口映射
目前只覆盖 Linux 宿主的 TCP。`exec`（进入运行中的容器）**只在 Linux 宿主可用**：
Linux 走 `setns`，Windows 没有对应原语，可行性取证见 PRD §4.9 W2。

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
