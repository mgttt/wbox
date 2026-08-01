# wbox

`wbox` 是一个不依赖硬件虚拟化的 portable 进程容器。近期主线是在 Windows、
Linux、macOS 三种宿主上运行 Linux OCI 镜像与少量 Windows CLI/PE 程序。
Windows 使用 AppContainer + Job Object，Linux 使用 rootless namespace/cgroup；
异构程序由第一方纯 Rust CPU/ABI runtime 承载。当前系统 Wine 路径仅是待删除 legacy。

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
| Windows × Windows 程序 | Sandboxie-Plus | **无任意路径写重定向、无注册表虚拟化**——原生 PE 程序发真 NT 调用，架构里没有介入点；已有默认拒绝、显式授权和 AppContainer package 私有标准目录 |
| Windows × Linux 镜像 | WSL2 / Docker Desktop | 已有 `build` 子集+分层缓存/`--restart`；无卷挂载/端口映射/`--user`，**性能不可比** |
| Linux × Linux 镜像 | Podman / Docker | 已有 `-v`/`-p`（仅 TCP）/`build`+分层缓存/`--restart`/`--user`（数字 id）/`--cap-add`/`--cap-drop`/`--seccomp-deny`（拒绝名单）/healthcheck/`--network container:`/overlay 可写层/`push`（原样回推，保留分层）/`diff`/`commit`/`pause`/`save`·`load`/`export`·`import`/`cp`/`stats`/`restart`/`rename`·`prune`/`logs -f`·`--tail`/`ps -q`·`rm -f`/`images -q`/**命名卷**/`--entrypoint`·`--env-file`/`ADD`/**多阶段构建**/`ps --filter`/`compose` 子集/IPC·UTS 隔离与共享；无自定义 bridge 网络与内建 DNS、镜像分层存储 |
| Linux × Windows 程序 | Wine（能力基线） | 第一方 Rust PE/Win32 runtime 规划中；当前系统 Wine 路径为待删除 legacy，不计入 Rust-only 完成状态 |

每格**怎么跑起来的**（两条隔离链路 × 两种程序格式）见 `PRD.md` §2.4.1；
与四个参照物（Sandboxie-Plus / WSL2 / Podman·Docker / Wine）的**架构对照**
——对标物怎么做的、差在哪、为什么——见 §2.4.2；
每格的**下一步**（哪些缺口打算补、哪些永远不补）见 §2.4.3；
四格**等深度的能力路线图**（Q1 的条目多数待 Windows 侧验证）见 §2.4.4。

**两条硬天花板**（不说破的话"对标"只是口号）：

1. wbox **不装内核驱动**——这是"免安装、不要管理员权限"的直接代价。因此
   Windows 程序沙箱达不到 Sandboxie（minifilter）级别的写重定向完整性。
2. 没有虚拟化时，Windows 上跑 Linux 镜像靠用户态执行，**性能与 WSL2 不是一个
   量级**；定位是"没有 VT-x/WSL2 时仍然能跑"。

逐条能力对照见 [PRD.md](PRD.md) §2.4，天花板见 §2.5。

## 能力边界

| 能力 | Windows 宿主 | Linux 宿主 | macOS 宿主 |
|---|---|---|---|
| 运行宿主 CLI 程序 | AppContainer + Job Object | user/PID/mount namespace | 原生隔离待取证 |
| 运行 Linux OCI 镜像 | `wbox-linux.exe` 用户态执行 | `pivot_root` 后原生执行 | `wbox-linux` + 外层隔离，规划中 |
| 运行 Windows CLI 程序 | 原生执行 | 第一方 Rust Win32 runtime 规划中；Wine 为 legacy | 同一第一方 runtime，规划中 |
| CPU/内存/进程数限制 | Job Object | cgroup v2，受限时明确回退或拒绝 | 待真机取证 |
| 默认网络 | AppContainer 无网络 capability | 独立空 netns | 待真机取证 |
| 进程树清理 | Job kill-on-close | namespace/PDEATHSIG | 待 `agenterm-platform` 机制接入 |
| 后台运行与生命周期 | `--detach` / `ps` / `logs` / `stop` / `rm` | 同左，另有 `exec` | 规划复用同一状态模型 |

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
- [docs/rust-rewrite.md](docs/rust-rewrite.md)：`wbox-linux` 纯 Rust 模拟器的
  架构、已验证范围与剩余缺口。
- [plan/agi.md](plan/agi.md)：廉价 PFLOPS、异构/分布式算力与 AGI 基础设施的
  长期实验假设、计量方法和演进路线。
- [tests/KNOWN-FAILURES.md](tests/KNOWN-FAILURES.md)：guest 回归问题台账。
- [CHANGELOG.md](CHANGELOG.md)：按版本记录的已交付变更。

开发任务先读 `PRD.md`。代码与文档冲突时，以代码、测试和 CI 配置为事实，
并在同一变更中修正文档。
