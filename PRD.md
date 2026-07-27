# wbox Product Requirements Document

> 本文是项目需求、范围和进度的唯一总入口，主要读者是维护代码的 LLM agents。
> 用户用法见 `README.md`，实现原理见 `docs/architecture.md`，验证命令见
> `docs/testing.md`。最后更新：2026-07-27。

## 1. Agent 读取协议

开始任务时按以下顺序建立上下文：

1. 阅读本文，确认产品边界、功能状态和当前工作。
2. 按任务阅读 `docs/architecture.md`、`docs/testing.md` 或
   `vendor/blink/WIN32-PORT.md`，不要无差别加载全部历史。
3. 查看 `git status`、近期提交和相关代码。仓库可能有其他 agent 的并行改动，
   不覆盖、不回退不属于当前任务的修改。
4. 实现后运行与改动范围匹配的测试。有可独立交付的进展时提交并推送 `main`；
   冲突由当前 agent 基于双方意图解决。
5. 功能状态发生变化时更新本文；发布历史写入 `CHANGELOG.md`，不要在多份文档
   复制同一组动态数字。

事实发生冲突时，优先级为：当前代码和可重复测试 > CI 配置 > 本文 >
技术参考 > `CHANGELOG.md` 历史段落。

状态标记：

- `[done]`：实现和要求的验收均已完成。
- `[active]`：主路径已实现，但仍有明确缺口或正在补充验证。
- `[planned]`：认可的后续范围，尚未进入交付。
- `[out]`：非目标，除非产品范围被明确修改。

## 2. 产品定义

### 2.1 目标

在没有 VT-x/AMD-V、WSL2 或 Hyper-V 的机器上，为 CLI/TUI 工作负载提供一个
免安装、默认无需管理员权限的统一运行入口：

```text
wbox
├── Windows 宿主
│   ├── Windows 程序 -> AppContainer + Job Object
│   └── Linux ELF/OCI -> AppContainer + Job Object + wbox-linux
└── Linux 宿主
    ├── Linux 程序/OCI -> rootless namespace + cgroup/rlimit
    └── Windows CLI -> 同一 Linux 隔离层 + Wine
```

### 2.2 核心价值

1. **Portable**：Windows 发布物是可直接复制的 `wbox.exe` 与
   `wbox-linux.exe`，不安装服务或驱动。
2. **默认约束**：默认断网、最小环境变量、进程树随父进程回收；限制无法生效时
   必须明确报错，不允许静默裸跑。
3. **统一入口**：宿主程序和 OCI 镜像共用 `wbox run`、资源参数与退出码语义。
4. **可验证**：Windows 真机、Linux、Wine 和 guest syscall 行为均有自动门禁。

### 2.3 非目标

- `[out]` VM、Hyper-V、Windows Container/Silo 的替代实现。
- `[out]` 文件系统 overlay、注册表重定向或 minifilter 驱动。
- `[out]` GUI/DirectX/COM/Windows 服务和内核驱动工作负载。
- `[out]` 完整网络命名空间、NAT、端口映射和流量策略。
- `[out]` Docker daemon API、镜像构建、Compose 或 Kubernetes 兼容。
- `[out]` 未声明的弱化运行；缺少隔离前置时不得悄悄直接执行。

## 3. 用户与场景

### 3.1 主要用户

- 受管 Windows 机器上的开发者和自动化 agent。
- 不能启用虚拟化或安装驱动，但需要约束 CLI 程序的操作者。
- 需要在 Windows 上直接检查或运行 Linux x86-64 CLI/OCI rootfs 的用户。
- 希望 Windows 与 Linux 使用相近命令和限制语义的 CI/测试系统。

### 3.2 核心场景

```text
S1 运行不受信任或行为未知的 Windows CLI
├── 默认无网络
├── 限制内存、CPU 和进程数
└── wbox 退出时清理整个进程树

S2 在 Windows 上运行 Linux OCI 镜像
├── 从 registry 拉取并校验镜像
├── 合并 Entrypoint/Cmd/Env/WorkingDir
├── 给 AppContainer 下放 rootfs 只读执行权限
└── 由 wbox-linux 执行 Linux x86-64 ELF

S3 在 Linux 上运行宿主程序或 Linux 镜像
├── rootless user/PID/mount namespace
├── 镜像模式 pivot_root；宿主程序模式不换根
├── 默认新建空 netns
└── cgroup v2 优先，rlimit 仅作语义允许的回退

S4 在 Linux 上运行 Windows CLI
├── 识别 PE
├── 复用 S3 的隔离与限制
└── 调用系统 Wine，不自行实现 Win32
```

## 4. 功能需求树

### F1 CLI 与运行目标分派 `[done]`

```text
F1
├── F1.1 `run -- <CMD>` 运行宿主程序
├── F1.2 `run <IMAGE> [-- CMD]` 运行已缓存镜像
├── F1.3 `--pull` 在缓存缺失时拉取镜像
├── F1.4 `image pull/list/show/rm`
├── F1.5 参数、子进程和内部错误退出码稳定
└── F1.6 `src/cli/mod.rs::USAGE` 是帮助文本唯一来源
```

验收：

- Windows 路径、镜像引用、显式 `--` 和参数转义不会互相误判。
- 子进程退出码原样返回；参数/profile/job/spawn/image 错误有固定分类。
- `--memory`、`--cpu-pct`、`--max-procs`、网络和环境参数跨后端语义一致。

### F2 Windows 原生进程容器 `[done]`

```text
F2
├── F2.1 创建或复用 AppContainer profile
├── F2.2 默认 Low IL 且无网络 capability
├── F2.3 `--allow-network` 授予 INTERNET_CLIENT
├── F2.4 Job Object 设置内存/CPU/进程数上限
├── F2.5 挂起创建 -> 加入 Job -> 恢复，消除启动前窗口
├── F2.6 KILL_ON_JOB_CLOSE 回收进程树
└── F2.7 默认删除 profile，`--keep-profile` 可保留
```

验收：

- 普通用户可运行，不依赖 `SeAssignPrimaryTokenPrivilege`。
- profile、capability SID、Job 限额、命令行保真及完整启动链在 Windows CI
  运行，不以权限理由静默跳过。

### F3 OCI Distribution 与本地镜像缓存 `[done]`

```text
F3
├── F3.1 Docker 风格引用补全与 registry override
├── F3.2 Bearer token / Basic 凭证交换
├── F3.3 manifest list 按 OS/arch 选择
├── F3.4 manifest/config/layer SHA-256 链式校验
├── F3.5 gzip tar / tar、whiteout、opaque、硬链接
├── F3.6 路径穿越与 symlink 越界拒绝
├── F3.7 Windows 无 symlink 权限时降级复制
├── F3.8 缓存 list/show/rm 与敏感 Env 脱敏
└── F3.9 Entrypoint/Cmd/Env/WorkingDir 合并
```

验收：

- digest 不匹配、越界路径和越权认证端点必须失败。
- 网络不可达可在网络型测试中记 SKIP，但本地构造的严格错误路径必须通过。
- 缓存目录按 registry/repository/reference 隔离，重复 pull 不混入旧 rootfs。

### F4 Windows 上执行 Linux ELF `[active]`

```text
F4
├── F4.1 wbox-linux.exe 可静态分发
├── F4.2 x86-64 指令解释/JIT 与 Linux syscall 翻译
├── F4.3 rootfs 前缀、路径边界和 /proc、/dev 兼容
├── F4.4 文件、目录、mmap、epoll、socket、timer/event/signal fd
├── F4.5 快照式 fork、exec、管道、shell 作业
├── F4.6 DNS、TCP/UDP、AF_UNIX 和 apt/wget 基础链路
└── F4.7 guest fd/VFS fd 在 fork 后保持命名空间一致
```

已完成主流单线程 CLI、动态 glibc 程序、shell 管道/命令替换/后台任务、
fork 子 DNS 和 `apt-get update`。仍有限制：

- 宿主异步信号语义不完整。
- glibc pthread/通用 clone 尚未支持。
- ptrace 未支持。

验收基线由 `tests/run.sh` 裁决；技术范围见
`vendor/blink/WIN32-PORT.md`，问题台账见 `tests/KNOWN-FAILURES.md`。

### F5 Linux 原生后端 `[done]`

```text
F5
├── F5.1 rootless user namespace，容器内 uid 0
├── F5.2 PID namespace，guest 为 PID 1
├── F5.3 镜像模式 pivot_root
├── F5.4 宿主程序模式保留宿主文件系统
├── F5.5 默认空 netns；`--allow-network` 共享宿主网络
├── F5.6 cgroup v2 memory/pids/cpu
├── F5.7 语义可等价时使用 rlimit 回退
└── F5.8 父进程死亡后清理后代
```

每一项都有对应门禁用例，可逐条核对（`scripts/test-linux-backend.sh`）：

| 条目 | 门禁用例 |
|---|---|
| F5.1 uid 0 | L1.1 |
| F5.2 PID 1 | L1.3（镜像）/ H.1（宿主） |
| F5.3 pivot_root | L1.2（宿主文件系统不可见）|
| F5.4 宿主模式保留宿主 FS | H.2（与 L1.2 互为反证）/ H.3 / H.5 |
| F5.5 默认空 netns | N.1 / N.2 / N.3（断网时 loopback 仍可用）|
| F5.6 cgroup v2 | C.1 / C.2（CI 现造委派子树，`WBOX_LBE_CGROUP=1`）|
| F5.7 rlimit 回退 | L2.1 / L2.2 / L2.3 |
| F5.8 清理后代 | L3.1（宿主）/ L3.2（镜像）|

F5 转 `[done]` 的依据是这张表：八项各有一条持续执行的断言，而不是"实现过
一次"。此前 cgroup v2 一项长期零覆盖，正是因为把"跑通过"当成了"有覆盖"。

namespace、网络默认、两种文件系统模式、生命周期、rlimit 兜底与 **cgroup v2
首选路径**均已实现并进入 CI 门禁。

cgroup v2 的旧布局（在 wbox 自身所在 cgroup 下建受限子节点）已被实机取证
证伪（违反 no-internal-process 规则，EBUSY/EIO 双向堵死），现已改为：优先把
受限 target 建成 wbox 所在 cgroup 的**兄弟**（谁都不用挪，因而不受"调用方
shell 留在同一 cgroup"影响）；父级不可写时退回 supervisor/target 双 leaf；
再不行才 rlimit。CI 现造委派子树做门禁（`WBOX_LBE_CGROUP=1`），实测输出
`memory.max=16777216 memory.swap.max=0`、guest cgroup 为兄弟位置、退出后目录
已回收。

`--memory` 在 cgroup 路径下必须同时写 `memory.swap.max=0`，否则它只限常驻
内存、超出部分换出去照跑，与 `RLIMIT_AS` 直接拒绝分配的语义不一致——这一点
是门禁抓出来的。任何回退必须打印原因；`--cpu-pct` 等无法等价回退的限制应
拒绝，不能忽略。

### F6 Linux 上执行 Windows CLI `[done]`

```text
F6
├── F6.1 宿主模式识别 PE
├── F6.2 查找 `WBOX_WINE`、wine64 或 wine
├── F6.3 使用独立默认 WINEPREFIX
├── F6.4 复用 F5 的 namespace、网络和限额
└── F6.5 镜像模式遇到 PE 明确拒绝
```

验收由 `scripts/test-linux-backend.sh` 的 W 段及独立 Wine CI job 完成。

### F7 环境与凭证边界 `[done]`

- 默认只继承运行所需白名单。
- `--env-pass-all` 仍不得透传 `WBOX_*`、`BLINK_*` 等内部控制键。
- 镜像 Env、宿主 Env 和强制 Env 有明确优先级，`BLINK_PREFIX` 必须由 wbox
  覆盖。
- verbose/show 输出对密码、token、secret 等值脱敏。
- registry 凭证只发送给获准的认证端点。

### F8 运维型容器生命周期 `[planned]`

`--detach`、`wbox ps/stop/rm/exec/logs` 暂未排期。进入开发前必须先定义：

- 跨进程发现 Job/profile 的方式。
- 日志与 stdio 的持久化模型。
- 崩溃恢复及名称冲突语义。
- Windows 与 Linux 的可对齐范围。

## 5. 非功能需求

### N1 可移植性

- Rust stable；Windows 主目标 `x86_64-pc-windows-msvc`。
- `wbox-linux.exe` 使用 UCRT64 MinGW 构建并静态链接运行库。
- 不引入后台服务、驱动、tokio 或 clap。
- release 保持单文件可复制；完整 Windows 包仅含两个 exe 及校验文件。

### N2 失败语义

- 不能满足承诺的隔离或限制时明确失败。
- 资源创建采用 RAII 或显式回滚，不遗留 profile、进程、句柄、映射和缓存半成品。
- 外部网络抖动可以 SKIP；确定性功能错误不可转成 SKIP。

### N3 兼容性

- Windows 10/11/Server 和 Linux x86-64 为目标宿主。
- Linux guest 目标是常见 x86-64 CLI，不承诺完整内核 ABI。
- GUI、驱动、内核模块和依赖硬件特性的程序不在兼容范围。

### N4 可维护性

- Win32 unsafe 调用集中在平台模块，并说明 Safety 前提。
- CLI 帮助、错误码、功能状态和测试基线各有唯一事实源。
- 行为修复必须带最小回归；共享 fd、进程、映射或路径逻辑需覆盖 fork/失败回滚。
- 禁止用固定已知失败掩盖已修复行为，基线变好时必须同步收紧。

## 6. 当前状态

状态日期：2026-07-27，分支：`main`，版本：`1.0.0-rc2` 后续滚动开发。

| 工作流 | 状态 | 最近可信信号 |
|---|---|---|
| Windows 原生容器 | done | AppContainer、Job、启动链进入 Windows CI |
| OCI pull/cache/config | done | Rust 单测与 Windows pull 冒烟 |
| Windows Linux guest | active | 20/20 guest 文件；1217 pass、0 fail、9 skip |
| Windows shell 矩阵 | done | 43 pass、0 fail、1 skip |
| Rust 主机逻辑 | done | 最近记录 198 pass、0 fail |
| Linux 原生后端 | done | namespace/rlimit/cgroup v2 三条路径均已门禁；cgroup 改为兄弟 leaf 后实测生效 |
| Linux Wine 路径 | done | 独立 CI job 覆盖 PE、参数、退出码和网络语义 |
| 后台生命周期管理 | planned | 尚未设计 |

上述数字是该日期的状态快照，不作为门禁配置。真实基线分别以测试 runner、
`tests/known-failures.txt` 和 `.github/workflows/ci.yml` 为准。

## 7. 里程碑与时间线

```text
2026-07-23
└── 验证 blink 路线，确定 Windows 上运行 Linux ELF 的架构

2026-07-24
├── OCI/rootfs 与动态 glibc 基础链路
└── 网络、shell 和 apt 场景验证

2026-07-25
├── 快照式 fork/exec、内存和 fd 语义集中实现
└── guest C 回归体系扩展

2026-07-26
├── v1.0.0-rc1 / rc2
├── Windows 真机 CI、portable 双 exe 和发布门禁打通
└── AF_UNIX、eventfd/timerfd/signalfd、epoll、长路径等真机差异收敛

2026-07-27
├── fork 后 guest/VFS fd 命名空间继续收敛
├── Linux Wine 执行路径落地
├── Linux cgroup v2 委派布局取证
└── 文档收敛为 PRD + 技术参考
```

下一里程碑不使用虚构日期，按验收条件推进：

1. `[done]` Linux cgroup v2 改为兄弟 leaf（父级不可写时退回 supervisor/target
   双 leaf），CI 现造委派子树做门禁，已取得实际限额证据。
2. `[active]` 继续补齐 wbox-linux fork 后 fd、socket 和资源失败回滚边界。
3. `[planned]` 决定是否发布新的 rc；要求全部发布门禁通过且 PRD 状态同步。
4. `[planned]` 是否进入 F8 生命周期管理，由新的需求决策单独启动。

## 8. 验收与发布

常规改动至少执行相关单测。跨平台、公共 fd/进程/路径逻辑或发布改动必须检查：

```text
release gate
├── test-linux                 Rust/Linux
├── test-windows               Rust/Windows 真机
├── check-windows-msvc         双目标 clippy/check
├── smoke-windows              AppContainer + Job 启动链
├── build-wbox-linux           UCRT64 构建 + Windows shell 矩阵
├── guest-tests                guest C 回归及已知失败基线
├── test-linux-backend         namespace/network/resource/lifecycle
└── test-wine-backend          Linux 隔离层内运行 PE
```

Tag 发布必须等待所有 required jobs 成功，并产出 `wbox.exe`、
`wbox-linux.exe`、portable zip 和 `SHA256SUMS.txt`。完整命令和 SKIP 规则见
`docs/testing.md`。

## 9. 需求变更规则

- 新功能先在本文加入场景、边界、功能节点和可测试验收，再实现。
- 修复不需要新增产品节点，但若改变支持范围或限制，必须更新对应节点。
- 状态从 `[active]` 改为 `[done]` 必须能指向自动测试或可重复真机记录。
- 不把一次性的调试过程堆进本文；原因与修复写 commit/CHANGELOG，长期操作知识
  写技术参考。
- 不在 README、架构手册和测试手册复制“当前进度”。它们只链接本文。
