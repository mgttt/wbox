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
6. **先看 §4.9 `[TODO-PLAN]`**：那里按宿主分派了待办条目。挑与**你这台机器
   能真实验证**的宿主相符的条目做；验证不了的不要硬写，改成那里的一个新条目
   交给对应宿主的 agent。

事实发生冲突时，优先级为：当前代码和可重复测试 > CI 配置 > 本文 >
技术参考 > `CHANGELOG.md` 历史段落。

状态标记：

- `[done]`：实现和要求的验收均已完成。
- `[active]`：主路径已实现，但仍有明确缺口或正在补充验证。
- `[planned]`：认可的后续范围，尚未进入交付。
- `[out]`：非目标，除非产品范围被明确修改。
- `[TODO-PLAN]`：跨宿主交接点，见 §4.9。条目上的 `[Windows agent]` /
  `[Linux agent]` 标明该由哪台机器上的 agent 接手。

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

范围随 §2.4 的对标基线做过一次调整：原先列为 `[out]` 的**文件系统重定向、
端口映射、镜像构建**已被对标要求拉回范围内（见 §2.4 的差距表），因此从这里
移出。仍然不做的是：

- `[out]` VM、Hyper-V、Windows Container/Silo 的替代实现——wbox 的前提就是
  这些都用不了。
- `[out]` **内核驱动**（含 minifilter）。这条不只是"暂不做"：它划定了
  §2.4 中 Windows 程序沙箱的**能力上限**，见那里的说明。
- `[out]` GUI/DirectX/COM/Windows 服务工作负载（Wine 下的 GUI 另议）。
- `[out]` Kubernetes 兼容与 Docker daemon 的线协议兼容——对标的是 **CLI 与
  运行时行为**，不是做一个 drop-in 的 daemon。
- `[out]` 未声明的弱化运行；缺少隔离前置时不得悄悄直接执行。

### 2.4 对标基线

四个象限各有明确参照物。**列参照物不等于承诺功能对等**——每格逐条列出参照物的
特征能力与 wbox 的实际状态，能力上限受 §2.3 约束时如实标注。

状态记法：`有` = 已实现且有持续门禁；`部分` = 可用但有明确缺口；
`无` = 未实现；`不做` = 撞天花板或属非目标。

#### Q1 Windows 宿主 × Windows 程序 —— 对标 Sandboxie-Plus

| 参照物特征能力 | wbox | 说明 |
|---|---|---|
| 进程隔离与降权 | 有 | AppContainer SID + 低完整性级别 |
| 默认断网 | 有 | 不授 `INTERNET_CLIENT` capability |
| 资源限额（内存/CPU/进程数）| 有 | Job Object；Sandboxie 本身反而不强调这块 |
| 进程树可靠回收 | 有 | Job `KILL_ON_JOB_CLOSE` |
| 生命周期（ps/stop/rm/logs/exec/inspect/wait）| 有 | F8 全套 |
| **文件系统写重定向（copy-on-write）** | **不做** | Sandboxie 用 minifilter 驱动；wbox 不装驱动（天花板一）。取证见 §4.9 W3 |
| **注册表虚拟化** | **不做** | 同上 |
| 命名沙箱的持久化内容 | 无 | 没有写重定向，就没有"沙箱内容"这个概念 |
| 强制程序入沙箱（Forced Programs）| 无 | 需要驱动或全局钩子 |
| GUI 程序沙箱 | 不做 | §2.3 非目标 |

**这一格是四象限里差距最大的**，且差距的主因不是工作量而是架构前提：
不装驱动就做不到驱动级别的重定向完整性。

#### Q2 Windows 宿主 × Linux 镜像 —— 对标 WSL2 / Docker Desktop

| 参照物特征能力 | wbox | 说明 |
|---|---|---|
| 运行 Linux OCI 镜像 | 有 | `wbox-linux` 用户态执行；已实机跑通 Alpine/Ubuntu |
| 免虚拟化 | **有，且这是 wbox 存在的理由** | WSL2 要 Hyper-V，wbox 不要 |
| 双层隔离 | 有 | AppContainer 套模拟器 |
| 可写 rootfs 层 | 有 | 私有可写层（远端已实现） |
| **接近原生的性能** | **不做** | 用户态解释/JIT，天花板二 |
| 卷挂载 `-v` | 无 | 受天花板一牵连：Windows 侧无路径重定向手段，`-v` 明确报错 |
| 端口映射 `-p` | 无 | Linux 侧的用户态转发依赖 `setns`，Windows 无对应原语 |
| 镜像构建 | 部分 | F9.3 子集；Windows `RUN` 经 AppContainer + Blink，WP.18；无分层缓存 |
| 完整 syscall 覆盖 | 部分 | 缺口见 F4：异步信号语义、glibc pthread/clone、ptrace |
| systemd / 服务 | 不做 | 非目标 |

#### Q3 Linux 宿主 × Linux 镜像 —— 对标 Podman / Docker

**四象限里最接近对标的一格。**

| 参照物特征能力 | wbox | 说明 |
|---|---|---|
| rootless 运行 | 有 | user/PID/mount/net namespace |
| 资源限额 | 有 | cgroup v2 首选，受限时明确回退或拒绝 |
| `run/exec/ps/logs/stop/rm/inspect/wait` | 有 | F8 全套 + 远端补的 inspect/wait |
| `--detach` | 有 | |
| 卷 / 绑定挂载 `-v` | 有 | F9.1，含 `:ro` |
| 端口映射 `-p` | 部分 | F9.2，**仅 TCP**；UDP/ICMP 做不到 |
| 镜像 pull/list/show/rm/inspect | 有 | |
| 镜像构建 | 部分 | F9.3 子集 + **分层缓存**（F9.5）；`FROM` 仍整份复制，无 overlay |
| overlay 分层存储 | 无 | rootless 下 overlayfs 未必可用 |
| 镜像 push | 无 | |
| compose / pod | 无 | |
| restart policy | 有 | F9.6：`no`/`on-failure[:N]`/`always`（门禁 R.1–R.4）|
| healthcheck | 无 | |
| 自定义网络、容器间通信、内建 DNS | 无 | 当前只有"空 netns"与"共享宿主网络"两档 |
| `--user UID[:GID]` | 部分 | F9.7：数字 id 生效（门禁 U.1–U.4）；**只映射一个 id**，用户名不支持 |
| `--cap-add` / seccomp 剖面 | 无 | rootless 下语义与 docker 不同，需先定契约 |
| Docker daemon 线协议兼容 | 不做 | §2.3 非目标；对标的是 CLI 与运行时行为 |

#### Q4 Linux 宿主 × Windows 程序 —— 对标 Wine

| 参照物特征能力 | wbox | 说明 |
|---|---|---|
| 运行 Windows CLI 程序 | 有 | 复用 Linux 隔离层调用系统 Wine |
| PE 判定与误判防护 | 有 | 看完整签名而非只看 `MZ`（门禁 W.4/W.5）|
| 在隔离内运行（Wine 本身不提供）| **有，这是 wbox 的增量** | Wine 只做 ABI 翻译，不做隔离 |
| 自带 Wine | 无 | 依赖宿主已装；缺失时明确报错 |
| `wineprefix` 与宿主隔离 | 有 | 用专用的 `~/.wbox/wineprefix`，不碰用户自己的 `~/.wine` |
| `wineprefix` **容器之间**隔离 | 有 | 每容器一个 prefix，置于其状态目录内，随容器记录一并清理（§4.9 L2）|
| GUI / DirectX / .NET | 不做 | §2.3 非目标（Wine 下 GUI 另议）|

### 2.5 两条硬天花板

不说破这两条，"对标"就只是口号：

1. **不装内核驱动**（§2.3）。直接后果：Q1 的文件/注册表写重定向做不到
   Sandboxie 级别的完整性，并牵连 Q2 的卷挂载。这是"免安装、不要管理员权限"
   这一产品前提的代价，不是待办事项。
2. **无虚拟化时性能不可比**。Q2 靠用户态解释/JIT，定位是"没有 VT-x/WSL2 时
   仍然能跑"，不是性能对标。

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
├── 共享 rootfs 缓存保持只读，运行前创建容器 SID 专属可写副本
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

### 4.0 完成定义与需求追踪

测试证据分五级。级别描述的是**实际经过的调用链**，不是测试文件叫什么：

| 级别 | 含义 | 可以证明 | 不能证明 |
|---|---|---|---|
| G0 | 静态检查或单元测试 | 解析、换算、局部错误路径 | OS API、进程和产品链路 |
| G1 | 组件可执行文件直测 | 单个后端/模拟器行为 | `wbox` 编排及外层隔离 |
| G2 | 后端集成测试 | 一个后端经过真实 OS 原语 | 用户公开 CLI 的跨模块组合 |
| G3 | 产品路径测试 | 从 `wbox` 公开 CLI 到最终 workload | 发布包可复制性 |
| G4 | 发布形态测试 | 仅使用最终打包文件完成 G3 | 无 |

状态规则：

1. 用户可见功能节点只有具备持续执行的 G3 证据才能标 `[done]`；可移植发布要求
   G4。内部算法节点可由 G0/G1 裁决，但其父级仍需产品路径证据。
2. 每条 `[done]` 必须能同时指向实现入口、测试 ID、CI job 和最近一次成功提交。
   缺任一项、允许确定性 SKIP、或当前 `main` 对应 job 为红，状态就是 `[active]`。
3. 父节点只有在全部子节点满足完成定义时才能 `[done]`。历史人工记录和通过数量
   是诊断材料，不是状态依据。
4. 核心 G3/G4 使用仓库内离线夹具，不依赖 registry 或公网。公网测试只补充网络
   兼容性，失败可以注明环境原因，但不能替代离线产品门禁。

当前追踪矩阵：

| 需求 | 实现入口 | 已有最高证据 | 持续门禁 / 缺口 |
|---|---|---|---|
| F1.1/F1.5/F1.6 原生运行、退出码、帮助 | `src/cli`、`src/error.rs` | G3 Windows/Linux | Rust tests、`WN.1-WN.8`、L/H；退出码已有行为断言 |
| F1.2/F1.3/F1.4 镜像运行、pull、管理 | `src/cli/run.rs`、`src/oci` | G3 | `WP.3` 持续覆盖离线缓存到 Linux guest；pull 失败原子性仍缺门禁 |
| F1.7 Docker/Podman 基础 CLI 兼容 | `src/cli` | G3/G4 部分 | 别名与参数解析单测；生命周期兼容命令进入 P.25/WP.21，其他新增项仍须逐项进入产品门禁 |
| F2.1/F2.2/F2.5/F2.7 profile/token/启动 | `token.rs`、`sandbox.rs` | G3 | Windows Rust tests + `WN.1-WN.8` + `WP.1` |
| F2.3 Windows 网络放行 | `token.rs` | G3 | `WNET.1-WNET.4` 对照宿主、默认拒绝和 `--allow-network` |
| F2.4 Windows 资源限制 | `job.rs` | G2 | 只证明 Job API 接受参数；缺超限 workload 行为断言 |
| F2.6 Windows 进程树回收 | `job.rs`、`sandbox.rs` | G4 | WP.9、WP.15、WP.17 分别覆盖 stop、并发 exec 与 supervisor 崩溃后的整树回收 |
| F3.1-F3.4 引用、认证、manifest、digest | `src/oci` | G2 | Rust 严格错误测试 + 可选 pull；真实 pull 不能替代离线失败路径 |
| F3.5-F3.7 层、链接和路径 | `oci/image.rs` | G2 | 构造 tar 与真实 Alpine 3.20 applet 链接通过；dangling symlink 仍有缺口 |
| F3.8/F3.9 缓存管理与 config 合并 | `src/oci`、`cli/image.rs` | G2 | 缓存仅以 `rootfs` 目录判完成，失败/并发 pull 原子性未门禁 |
| F4.1 静态 `wbox-linux.exe` | build script | G1 | CI 构建后直跑；G4 两文件 bundle 由 `WP.3` 裁决 |
| F4.2-F4.7 ELF/syscall/fork/network/fd | `vendor/blink` | G1 | `test-matrix.sh`、guest C 套件均直接跑模拟器 |
| F4 Windows 完整 Linux guest 路径 | `BlinkBackend` + F2/F3/F4 | G3 | `WP.3`：portable artifact 在 AppContainer 内执行静态 BusyBox |
| F5.1-F5.5 namespace/fs/network | Linux backend | G3 | L1/H/N，CI 使用 REQUIRE |
| F5.6/F5.7 cgroup/rlimit | `linux_limits.rs` | G3 正常路径 | C/L2；溢出、spawn 失败清理和跨后端内存语义仍有缺口 |
| F5.8 后代清理 | `linux_ns.rs` | G3 | L3.1/L3.2 |
| F6.1-F6.3/F6.5 PE/Wine 分派 | `wine.rs` | G3 | W.1/W.2/W.4/W.5 |
| F6.4 隔离、网络和限额复用 | F5 + `wine.rs` | G3 部分 | W.3 覆盖网络；缺 PE workload 的资源超限行为断言 |
| F7.1-F7.5 环境与凭证 | `backend/env.rs`、`registry.rs` | G2/G3 部分 | Rust 严格测试 + `WP.2`；Linux image 与 Windows image 路径仍随各自 G3 |
| F8.1 状态目录与 `ps` | `runstate.rs`、`cli/ps.rs` | G3 | P.1-P.5、`WN.8`、`WNET.4` 与 `WP.5` |
| F8.2/F8.3 detach/logs/stop/rm | `src/cli/run.rs`、`logs.rs`、`stop.rs`、`runstate.rs` | G4 Windows / G3 Linux | P.6-P.18、WP.6-WP.12；`WP.7A` 新增 detached `--rm` |
| F8.4 exec | `src/cli/exec.rs` | G4 Windows / G3 Linux | Linux P.19-P.22；Windows 原生目标 WP.13-WP.17；CI 30250676453 通过 |
| F8.7 create/start | `src/cli/create.rs`、`start.rs`、`runstate.rs` | G3 Linux / G4 Windows | P.25/WP.21：create 不执行，start 原子领取配置，退出后可再次启动；提交 `1caada0`、CI 30271007552 |

`WP.*` 是 `scripts/test-windows-product.ps1` 的产品门禁：

- `WP.1`：最终 bundle 中的 `wbox.exe` 运行 Windows 原生程序。
- `WP.2`：公开 CLI 的环境边界及正常退出状态清理。
- `WP.3`：只用最终两个 exe 和仓库内静态 ELF，从本地缓存执行 Linux guest。
- `WP.3D`：AppContainer 内的 Linux guest 能枚举非空目录，防止 Python 等运行时
  因 `fdopendir` 路径恢复失败而把标准库目录看成空目录。
- `WP.4`：bundle 中不存在运行时 DLL 或仓库路径依赖。
- `WP.5`：前台正常退出后状态目录无运行记录。
- `WP.6`：Windows detach 后可由 `ps` 观察，并可通过 `logs` 读取输出。
- `WP.7`：`rm` 删除已退出的 detached 记录。
- `WP.7A`：`run -d --rm` 退出后自动删除状态与日志。
- `WP.8`：detached Windows workload 建立 supervisor、guest、child 三层进程树。
- `WP.9`：`stop` 后三层专属 PID 全部消失，记录转为 exited。
- `WP.10`：重复 `stop` 已退出容器保持幂等。
- `WP.11`：`stop` 未知名称明确失败。
- `WP.12`：`rm` 删除 stopped 记录。
- `WP.13`：Windows 原生 `exec` 接受 Docker/Podman 位置参数形状并继承工作目录。
- `WP.14`：Windows 原生 `exec` 原样返回 guest 退出码。
- `WP.15`：并发长命 `exec` 时，`stop` 清空共享 Job 且控制器正常收尾。
- `WP.16`：已退出容器明确拒绝 `exec`。
- `WP.17`：强杀 supervisor 后，主 guest 与 exec guest 均由
  `KILL_ON_JOB_CLOSE` 回收。
- `WP.19/WP.20`：`top` 只列 Job 内 guest，`kill` 立即回收完整 Job 进程树。
- `WP.21`：`container create` 不执行 Windows workload；`container start` 启动，
  退出后再次 `start` 必须产生一代全新的 supervisor/guest/child 进程树。

`WN.*` 是 `scripts/test-windows-native.ps1` 的 Windows 原生程序矩阵：

- `WN.1-WN.4`：`cmd.exe`、Windows PowerShell 解释器/CLR、`hostname.exe`、`whoami.exe`。
- `WN.5`：AppContainer 内启动并等待子进程。
- `WN.6`：显式授权的工作目录可写，且宿主能读取结果。
- `WN.7`：非零 workload 退出码原样返回。
- `WN.8`：所有前台运行完成后 `ps --all` 无状态残留。

`WN.2` 只证明 Windows PowerShell 解释器与 CLR 可在默认 AppContainer 中运行。
GitHub Server 2025 runner 上，依赖宿主模块目录自动发现的 `Write-Output` 尚不可用；
标准 PowerShell 模块的跨宿主兼容性仍是 active 缺口，不得由该项外推为完整支持。

`WNET.*` 是 `scripts/test-windows-network.ps1` 的 Windows 网络行为门禁：

- `WNET.1`：宿主可访问同一个公网数值 IP 端点，排除端点或 runner 网络故障。
- `WNET.2`：默认 AppContainer 必须无法访问该端点。
- `WNET.3`：`--allow-network` 必须访问成功并收到非 `000` HTTP 状态。
- `WNET.4`：两次前台运行结束后 `ps --all` 无状态残留。

### F1 CLI 与运行目标分派 `[active]`

```text
F1
├── F1.1 `run -- <CMD>` 运行宿主程序
├── F1.2 `run <IMAGE> [CMD [ARG...]]` 运行镜像，缓存缺失时默认 pull
├── F1.3 `--pull` 作为 Docker/Podman 显式兼容写法
├── F1.4 `image pull/list/show/rm`
├── F1.5 参数、子进程和内部错误退出码稳定
├── F1.6 `src/cli/mod.rs::USAGE` 是帮助文本唯一来源
└── F1.7 Docker/Podman 基础 CLI 兼容层
    ├── F1.7.1 `pull <IMAGE>` 等价 `image pull <IMAGE>`
    ├── F1.7.2 `images` 与 `image ls` 等价 `image list`
    ├── F1.7.3 `rmi <IMAGE>` 等价 `image rm <IMAGE>`
    ├── F1.7.4 `ps -a`、`rm <NAME>...` 保持常见命令形状
    ├── F1.7.5 `run --name/-w/--workdir/--rm/-v` 接受常见参数拼法
    ├── F1.7.6 `run --network none|host` 映射 wbox 的默认断网与网络放行
    ├── F1.7.7 `exec NAME COMMAND [ARG...]` 不强制要求 `--` 分隔
    ├── F1.7.8 未实现参数必须明确拒绝，禁止静默忽略
    ├── F1.7.9 `kill [-s KILL] NAME...` 立即终止，不经过 stop 宽限期
    ├── F1.7.10 `top NAME` 列出隔离单元成员，不混入 wbox supervisor
    └── F1.7.11 `create [RUN OPTIONS]` 保存配置但不运行，`start NAME...` 启动或重启
```

验收：

- Windows 路径、镜像引用、显式 `--` 和参数转义不会互相误判。
- 首个镜像参数之后的 `-c`、`--name`、`-e`、`-w` 等全部原样属于 guest；
  未缓存镜像不得静默退化为宿主程序，明确本地程序使用 `run -- PROGRAM`。
- 子进程退出码原样返回；参数/profile/job/spawn/image 错误有固定分类。
- `--memory`、`--cpu-pct`、`--max-procs`、网络和环境参数跨后端语义一致。
- Docker/Podman 兼容只覆盖 wbox 能兑现的沙箱语义。端口发布、Windows bind
  volume、`--mount`、守护进程 API、compose/pod 和远程上下文不在当前兼容范围；
  收到这些参数时必须返回参数错误，不得假成功。

#### F1.7 Docker/Podman 兼容命令树

```text
wbox
├── run [兼容子集] IMAGE|-- PROGRAM [ARG...]
│   ├── 生命周期：--name、--rm、-d/--detach
│   ├── 工作目录/卷：-w、--workdir、-v host:guest[:ro|:rw]（Linux 宿主）
│   ├── 网络：--network none|host
│   └── wbox 扩展：--memory、--cpu-pct、--max-procs、--allow-network
├── create [run 兼容子集] IMAGE|-- PROGRAM [ARG...]
├── start NAME...
├── pull IMAGE              -> image pull IMAGE
├── images                  -> image list
├── rmi IMAGE               -> image rm IMAGE
├── image
│   ├── pull IMAGE
│   ├── ls|list
│   ├── show IMAGE
│   └── rm IMAGE
├── ps [-a|--all]
├── exec NAME [--] COMMAND [ARG...]
├── inspect|wait|logs|stop NAME...
├── kill [-s KILL|SIGKILL|9] NAME...
├── top NAME
├── container
│   └── create|start|ls|inspect|wait|logs|exec|rm|stop|kill|top
└── rm NAME...
```

兼容原则：

1. 命令名、常用短选项和参数位置优先贴近 Docker/Podman；同一输入在两者语义
   一致时，wbox 应给出等价结果。
2. wbox 默认前台运行、默认断网、退出即清理；`-d/--detach` 是显式后台模式。
   兼容参数不得暗中削弱这些默认边界。
3. Docker 与 Podman 语义不一致，或 wbox 后端无法兑现时，帮助和错误必须明确
   写出 wbox 的行为；不以“参数已接受”冒充功能兼容。
4. 每个新增兼容项至少具备 G0 解析测试；涉及隔离、网络、缓存或生命周期的项
   还必须进入对应 G3/G4 产品门禁后才能标记完成。
5. 顶层与基础子命令接受 `--help/-h`，并支持 `wbox help <COMMAND>`；镜像后的
   `--help` 仍属于 guest argv，不得被宿主帮助入口截获。

### F2 Windows 原生进程容器 `[active]`

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

### F3 OCI Distribution 与本地镜像缓存 `[active]`

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
├── F3.9 Entrypoint/Cmd/Env/WorkingDir 合并
└── F3.10 跨层目录头保留子 symlink，链式别名最终可物化
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
├── F4.7 guest fd/VFS fd 在 fork 后保持命名空间一致
└── F4.8 `[done]` 每容器临时可写层，退出清理且不修改共享镜像缓存
```

直接运行 `wbox-linux.exe` 的 G1 组件测试已覆盖主流单线程 CLI、动态 glibc
程序、shell 管道/命令替换/后台任务、fork 子 DNS 和 `apt-get update`。这些
结果不再表述为 Windows 产品路径已完成；`BlinkBackend` 经 AppContainer 的 G3
仍由 WP.3 裁决。组件层仍有限制：

- 宿主异步信号语义不完整。
- glibc pthread/通用 clone 尚未支持。
- ptrace 未支持。

**F4.3 的覆盖缺口（2026-07-27 发现并修复）**。`wbox run <镜像>` 走的是
`BLINK_PREFIX=<rootfs>`，而 `scripts/test-matrix.sh` 的 A–F 组**全部不设**
`BLINK_PREFIX`（guest 的 `/` 直通宿主 `/`）。也就是说产品首页宣传的
"Windows 上跑 Linux OCI 镜像"这条路径，长期**没有任何自动化覆盖**；
`test-windows-product.ps1` 的 WP.3 第一次执行时暴露出崩溃：

```
wbox-linux: fatal host exception 0xc0000005 (read) at rip=…
guest rip=0x4038b1 in /busybox   fault address FFFFFFFFFFFFFFFF
```

这不是新引入的回归，是一直存在、直到有门禁才暴露的缺陷。矩阵新增 G 组裸跑
`wbox-linux.exe` + `BLINK_PREFIX`，与 WP.3 只差一层 AppContainer。真 Windows
上 **G1/G2/G3 全部通过**——guest 绝对路径执行、换根隔离、退出码转发都正常
（G2 起初因 `ls` 多收到一个 `/SystemRoot` 参数而假失败，改为直接断言
rootfs 条目存在、宿主目录不可见后转绿，不再依赖解析 `ls` 输出）。

诊断补丁把 rc139 还原为确定错误：
`initial AllocatePageTable failed: errno=13 (Permission denied)`。根因是 Win32
缺少 POSIX `MAP_ANONYMOUS` 时，`PortableMmap` 用当前目录下的 `mkstemp`
模拟所有匿名映射；而 WP.3 的 OCI rootfs 按设计只有读执行权限。首个页表因此
无法创建 `blink.dat.XXXXXX`，release 构建又未检查返回值，最终表现为宿主异常。

修复后，Win32 私有匿名映射直接由 `W32Mmap64` 在 guest 虚拟地址窗口内 commit，
只有需要 snapshot-fork 文件身份的共享匿名映射保留临时文件路径；页表与映射
保护失败也会明确报错，不再进入未定义行为。CI `30238223406` 的
`WP.1-WP.17` 本地全部通过。同一 CI artifact 在 Windows 实机经 `wbox run` 启动
Alpine 3.20 的 `/bin/sh`，执行 `uname` 与读取 `/etc/alpine-release` 均为 rc0。

G 组本身也永久补上了这块覆盖——`wbox run <镜像>` 走的就是这条路，此前零覆盖。

**Ubuntu 24.04 真镜像取证（2026-07-27）**。Docker Hub 直连在本机超时，改用
`docker.m.daocloud.io` 后成功选择 linux/amd64 manifest，校验 config 与单层
digest，并解包约 29.7 MB rootfs。Windows 实机通过 `wbox run` 验证：

- `/bin/sh` 和动态 glibc 可启动，`/etc/os-release` 为
  `ID=ubuntu VERSION_ID=24.04`；
- `dpkg --print-architecture=amd64`、`getconf LONG_BIT=64`；
- guest 看不到宿主 `/Windows/System32`；
- guest `exit 37` 原样返回 37，前台状态无残留。

该取证发现两个边界。其一，Windows 无 symlink 权限时，Ubuntu 文档与 locale
中的部分悬空/后置目标链接会产生降级复制警告；核心 shell 未受影响，但这类
非关键链接不能宣称完整还原。其二，`uname -m` 一度错误输出构建年份 `2026`：
`SysUname` 用 `strcpy` 把过长构建元数据写入 Linux 固定 65 字节的 `version`
字段，覆盖了其后的 `machine`。实现已改为字段内有界 `snprintf`，组件矩阵新增
精确断言 `uname -m == x86_64`。CI 30253571295 的真 Windows 矩阵通过；同一
artifact 回灌本机后，Ubuntu 24.04 的 `uname -m=x86_64`、Bash、APT 2.8.3、
dpkg amd64、64 位 glibc、宿主文件系统隔离和退出码 37 透传全部通过，问题关闭。

**Fedora 42 / Python 3.12 Alpine 扩展取证（2026-07-27）**。两个镜像均由
`docker.m.daocloud.io` 拉取并通过 manifest/config/layer digest 校验。Fedora
单层镜像的 shell、os-release、`uname -m=x86_64` 与 RPM 架构通过；首次运行
`dnf --version` 暴露容器环境缺少 `HOME`，镜像默认环境现补为 `/root`，显式
`-e HOME=...` 仍优先。补齐 `HOME` 后 `dnf5 --version` 在 AppContainer 内外
均超过 10 秒无输出，排除 AppContainer 权限层后仍可复现；该项是独立的
Blink/Linux ABI、线程或同步原语兼容缺口，必须以有界超时门禁继续定位，当前
不得标记为通过。5 秒 `LD_DEBUG`/内存诊断证明动态链接已完成，进程进入 RPM
SQLite 初始化后反复打开 `rpmdb.sqlite-shm`，CPU 时间约 1.1 秒；下一步优先
核对 Win32 SQLite 共享内存、文件锁与 mmap 语义。恢复 syscall trace 后确认
循环为 `F_GETLK` 返回成功却未把查询结构改写为 `F_UNLCK`，SQLite 因而永久
误判写锁冲突；实现现已补齐回写、坏 fd 与空指针校验，并加入 guest 回归。
此前 Win32 release 的
`-s/-sss` 会被 `NDEBUG` 连带编译为空操作，现将 syscall trace 与普通 debug
日志解耦，并由 `WP.4S` 保证 release artifact 能输出 syscall 记录。

CI 30259159700 与回灌 artifact 证明 `F_GETLK` 回归通过，`dnf5` 不再超时；
它随后立即暴露 F4.8：AppContainer 对共享 rootfs 只有读执行权限，创建
`/root/.local/state` 或写 `dnf5.log` 返回 `EACCES`。共享镜像缓存不得直接授予
guest 写权限。F4.8 首版在取得容器注册锁后，把缓存复制到
`~/.wbox/run/<name>/rootfs`，只向该 profile 的确定性 AppContainer SID 授予
修改权。前台退出自动清理；后台退出保留到 `wbox rm`；`--rm` 立即清理。
`WP.3W/WP.3WB` 分别裁决前台写入、缓存不变、自动清理与后台保留/显式删除。
detached 父进程现在必须在 `.operations.lock` 内写入一次性预留令牌，
supervisor 凭同一令牌接管；`run/rm` 在接管前均把该名称视为运行中，禁止锁外
盲删状态树。supervisor 在 pull/copy/prepare 失败时负责撤销预留，`--rm`
从登记开始即采用自动清理语义。ACL 递归使用 `symlink_metadata` 并把所有
reparse point 当作不递归的叶节点，禁止链接把容器 SID 授权带出 rootfs；
私有副本中的绝对 symlink 目标则重写到该次运行的私有 rootfs 内。

Windows 实机复测中，Fedora 42 `dnf5 --version` 首次 rc0 并列出完整插件，
耗时约 43.9 秒；Python 3.12.13 import（含跨层 symlink 链）约 20.6 秒，
Ubuntu 24.04 glibc shell 约 21.9 秒，均 rc0，退出后无状态目录。首版全量复制
保证 create/write/rename/delete 的正确语义，但启动成本和磁盘放大明显；后续
可替换为完整 copy-up/whiteout 的稀疏层，不能退回修改共享缓存。

Python 四层镜像暴露两个独立问题：

1. 后层重复的 `usr/local/bin/` 目录头错误清除了前层的逻辑 symlink，
   令 `python -> python3 -> python3.12` 链只剩首段。目录条目现只替换同路径
   symlink，不再删除子项；真实重拉后 `python/python3/idle/pydoc/*-config`
   均已物化，新增跨层目录头回归。
2. AppContainer 内 `stat/cat` 标准库文件成功，但目录枚举为空；同一
   `wbox-linux.exe` 脱离 AppContainer 后正常。根因是 Win32 `fdopendir`
   依赖在 AppContainer 中可能被拒绝的 `GetFinalPathNameByHandleW`。Win32 fd
   现记录 `openat` 的规范宿主路径，并在 `dup/dup2/close` 同步生命周期；
   `WP.3D` 用真实 AppContainer 目录枚举裁决。CI 30257127594 中该门禁通过；
   同源 artifact 回灌 Windows 实机后，Python 3.12.13 成功加载
   `encodings/__init__.py`，`sys.executable=/usr/local/bin/python3`、
   `platform.machine()=x86_64`，该问题关闭。

验收基线由 `tests/run.sh` 裁决；技术范围见
`vendor/blink/WIN32-PORT.md`，问题台账见 `tests/KNOWN-FAILURES.md`。

### F5 Linux 原生后端 `[active]`

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

这张表证明了 F5 的正常主路径，但 2026-07-27 的追踪审计发现它没有覆盖限额
换算溢出、`spawn`/`wait` 失败后的 cgroup 清理，以及 Windows 每进程内存与
Linux 整组内存的语义差异。因此 F5 回到 `[active]`；补齐这些反例后才能重新
按 §4.0 的完成定义评为 `[done]`。

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

### F6 Linux 上执行 Windows CLI `[active]`

```text
F6
├── F6.1 宿主模式识别 PE
├── F6.2 查找 `WBOX_WINE`、wine64 或 wine
├── F6.3 使用独立默认 WINEPREFIX
├── F6.4 复用 F5 的 namespace、网络和限额
└── F6.5 镜像模式遇到 PE 明确拒绝
```

验收由 `scripts/test-linux-backend.sh` 的 W 段及独立 Wine CI job 完成。

### F7 环境与凭证边界 `[active]`

- F7.1 默认只继承运行所需白名单。
- F7.2 `--env-pass-all` 仍不得透传 `WBOX_*`、`BLINK_*` 等内部控制键。
- F7.3 镜像 Env、宿主 Env 和强制 Env 有明确优先级，`BLINK_PREFIX` 必须由 wbox
  覆盖。
- F7.4 verbose/show 输出对密码、token、secret 等值脱敏。
- F7.5 registry 凭证只发送给获准的认证端点。

### F8 运维型容器生命周期 `[active]`

`create/start`、`--detach`、`wbox ps/stop/rm/logs/exec/wait/inspect`。这是 wbox 离"能当 harness 的长期
环境"最近的一组能力；基础链路已经实现，当前重点是持续门禁与平台差异收敛。

四个前置问题的设计答复如下：

**F8.a 跨进程发现**（已实现，`src/runstate.rs`）。状态目录
`~/.wbox/run/<name>/`，内含 `meta.json` 与 `lock`。存活判定**不靠 pid**——
pid 会被复用，拿它判断迟早会把别人的进程当成自己的容器，而 `stop` 一旦据此
发信号就是杀错进程。

实现时改了原定方案：原打算 Linux 查 `cgroup.procs`、Windows 开具名 Job，
但那是**两套**机制，且 cgroup 那条只在 cgroup 路径可用（rlimit 兜底时没有
cgroup 可查）。最终统一用**锁文件**——两侧语义都由操作系统保证：进程无论
正常退出、崩溃还是被 SIGKILL，内核都会关掉它的 fd/句柄，锁随之释放，于是
"能拿到锁"精确等价于"没有活着的 owner"。Linux 用 `flock`（绑定 open file
description，同进程重开也冲突，故单测可在进程内验证；`fcntl` 记录锁按进程算
则不行），Windows 用 `share_mode(0)`。**同一套单测覆盖两侧**，Windows 那半
由 CI 的 windows runner 真实执行。

**F8.b 日志模型**（已实现）。`--detach` 时 stdout/stderr 落到状态目录下的
`stdout.log`/`stderr.log`，`wbox logs <NAME> [--stderr]` 读文件。stdin 不连接。
`-f` 跟随尚未实现。

上限的实现方式与其**边界**如实记录：supervisor 起一个看门狗线程每 500ms 检查
体积，超限即清空并写入一行截断说明（丢旧留新）。这是**周期采样，不是硬实时
上限**——两次 tick 之间容器可以短暂冲过。实测踩到过一个真问题：一个 300k 行、
1 秒内写完 2.5MB 就退出的容器**活不到第一次 tick**，整份输出原样落盘，上限
形同虚设。因此在容器退出后**再收一次尾**，最终落盘体积由此无论容器活多久都
有界。要做到任意时刻严格不超，只能在 wbox 与 guest 之间插一层转写，那要改动
全部 Backend 的 stdio 处理，是另一个量级的改动。

另一处必须写下的实现约束：日志文件以 **append 模式**打开。截断靠 `set_len(0)`，
而只有 append 写入才会在截断后从新的文件末尾（0）继续；普通写模式下 writer
自带偏移，截断后文件立刻变回原大小的稀疏文件，上限完全失效。

**退出后保留**：后台容器退出后记录与日志**不删**，`ps -a` 显示为 `exited`，
由 `wbox rm` 显式清理（与 docker 一致）。前台容器仍是退出即清理——它的输出
已经打在终端上了，留个空目录只是垃圾。这条差异是刻意的：一退出就删日志，
等于把 `logs` 最主要的用途（事后查看跑完的后台任务输出）废掉。

**F8.c 崩溃与重名**。wbox 自身崩溃时容器必死（Windows 的 kill-on-job-close
与 Linux 的 PDEATHSIG 都已保证），故状态目录可能残留而容器已亡——`ps` 据
F8.a 判活，把这类标为 `exited`，不假装还在。重名：目标存活则**报错**并提示
换名或先 `rm`；目标已亡则提示 `rm` 后重来，不自动覆盖（自动覆盖会让"我以为
在跑的那个"悄悄消失）。

**F8.c 补充：`stop` 的平台差异（已实现，如实记录）**。`stop` 终止的是
**supervisor（wbox 自己）**而非 guest——容器整棵树的存活绑在 supervisor 上
（Linux 的 PDEATHSIG、Windows 的 Job kill-on-close），杀 supervisor 内核就会
收走整棵树；直接杀 guest 反而漏掉它的子孙。

Linux 先 `SIGTERM` 后 `SIGKILL`（默认给 10 秒，`--timeout` 可调）。
**Windows 没有 SIGTERM 的等价物**——控制台事件对无控制台的后台进程不适用，
因此 `Graceful` 在 Windows 上等同于强制终止。这不是偷懒，是平台确实缺少对齐物。

退出判定**以锁为准而不是以 pid 为准**：pid 会被复用，而锁被释放才真正等价于
"持有它的那个 wbox 没了"。

`stop` 对已停止的容器**幂等**（不报错），否则 `wbox stop x` 在脚本里没法用；
但停一个**不存在**的容器仍然报错——那是"没这个东西"，与"已经停了"是两回事。

`kill` 与 `stop` 必须保持不同契约：`kill` 不等待 guest 自行清理，默认立即清空
Linux 进程树或 Windows 命名 Job；跨平台子集只接受 `KILL/SIGKILL/9`，其他信号
在实现前明确拒绝。`top` 只列隔离边界内的成员：Linux 从 `container.pid` 枚举
`/proc` 后代并隐藏 PID namespace 中间进程，Windows 直接查询命名 Job；两侧都
不得把宿主侧 supervisor 当作 guest。当前不接受 Docker/Podman 可追加的任意
宿主 `ps` 参数，避免参数看似成功但输出语义漂移。

`--restart` 每次拉起新 guest 前必须覆盖 `container.pid`（Linux R.5）；`exec/top` 读取当前值，
端口转发也必须在每条新连接建立时重新读取，而不能永久绑定第一次运行的
namespace owner PID。否则重启策略表面成功，管理面与网络面却仍指向已经退出的
上一代容器。

**F8.e create/start 状态机**。`create` 完成镜像解析、必要的 pull 与参数验证，
把运行参数保存到 `create.json`，但不得创建 workload、owner 锁或运行时 PID。
状态必须显示为 `created`、PID 为 0。`start` 在状态根操作锁内原子领取保存配置，
先转成 detached reservation，再启动 supervisor；并发 `start` 只能有一个成功。
supervisor 登记前失败必须恢复 `created`，不得留下假 running。登记后的退出状态为
`exited`，保存配置仍在，因此可再次 `start`；带 `--rm` 的配置退出后按约定删除。
`stop/kill/top/exec/wait` 对 created 状态明确拒绝，`rm` 可删除 created 记录。

`create.json` 会持久化显式 `-e` 参数，Linux 权限必须为 0600；不得把宿主隐式环境
或 registry 凭证写入 `meta.json`。使用者不应把长期凭证直接放在命令参数中。

**F8 的覆盖现状（如实记录）**。Linux 由 P.1–P.25 覆盖完整生命周期；
Windows 由 WP.6–WP.21 覆盖 detach、ps、logs、stop、rm、kill/top、create/start 与原生 exec，其中
WP.17 直接证明 supervisor 崩溃时主 guest 和 exec guest 均被 Job 回收。Windows
OCI/Blink exec 不在承诺范围，必须明确拒绝。

detached supervisor 在释放 owner 锁前把 guest 退出码写到状态目录的
`exit-code`；`wbox wait NAME...` 等待锁释放后打印该值，`inspect` 的
`State.ExitCode` 使用同一来源。异常崩溃和旧版残留没有可信退出码时必须报告
unknown，不能编造为 0。`wbox inspect`、`image inspect` 与
`container inspect` 输出 JSON 数组；镜像内疑似凭证的 Env 仍脱敏。

**F8.d 两侧可对齐范围**。`ps/stop/rm/logs/--detach` 语义可完全对齐。
`exec` 只能部分对齐：Linux 进入已有 namespace；Windows 原生目标重新使用同一
AppContainer SID、网络 capability 与命名 Job，并继承记录的工作目录。Windows
OCI/Blink 的 rootfs 与镜像环境无法可靠重建，明确拒绝。原生 exec 也不继承
原 run 的自定义环境：状态文件刻意不落环境变量或凭证；需要这类语义时应由未来
的 supervisor 控制通道传递，而不是把秘密写入 `meta.json`。

分期与验收（每期都要有持续执行的门禁断言，理由见 §6 的覆盖教训）：

| 期 | 范围 | 验收 |
|---|---|---|
| F8.1 `[done]` | 状态目录 + `wbox ps`（只读） | P.1–P.5、WN.8、WNET.4 与 WP.5 已通过；跨进程 register/rm 竞态 G0 与 CI 30250676453 通过 |
| F8.2 `[done]` | `--detach` + `logs` | **已完成**（门禁 P.9–P.14）：detach 立即返回、容器后台续跑、stdout/stderr 分别落盘可读、退出后保留记录供事后查看、体积有界且截断可见 |
| F8.3 `[done]` | `stop` / `rm` | **已完成**：`stop` 收走整棵进程树（P.15，3→0 后代）、状态转 exited 并保留（P.16）、幂等（P.17）、不存在时报错（P.18）；`rm` 拒绝删存活容器（P.6/P.7/P.8）|
| F8.4 `[done]` | `exec` | Linux P.19-P.22 与 Windows 原生 WP.13-WP.17 在 CI 30250676453 通过；Windows OCI/Blink 明确拒绝 |
| F8.5 `[done]` | `wait` + container/image `inspect` | Rust 跨平台状态测试；Windows 双 exe 产品路径 WP.7B/WP.7C |
| F8.6 `[done]` | `kill` + `top` | Linux P.23/P.24；Windows WP.19/WP.20；Windows `top` 查询 Job 成员，`kill` 清空三层进程树 |
| F8.7 `[done]` | `create` + `start` | Rust 原子状态机与 CLI 测试、Linux P.25、Windows WP.21 均通过；提交 `1caada0`、CI 30271007552 |

### F9 对标能力补齐 `[planned]`

按 §2.4 的**跨象限收益**排序，不按实现难度排。每项都要能落到门禁上，
否则又会变成"README 宣传了但没人跑过"的那类条目（F4.3 的教训）。

```text
F9
├── F9.1 卷 / 绑定挂载 `-v host:guest[:ro]`   —— [partial] Linux 已完成，Windows OCI 已取证
├── F9.2 端口映射 `-p`                        —— [done]（Linux 侧，仅 TCP）
├── F9.3 镜像构建（Dockerfile 子集）          —— [done]（Linux + Windows）
└── F9.4 Windows 文件系统写重定向             —— 单象限，且受 §2.4 天花板限制
```

**F9.1 卷 / 绑定挂载** `[partial]`（Linux 宿主已完成，门禁 V.1–V.4）。已定的语义：

- 只读/读写：`:ro` / `:rw`（默认读写）。**`:ro` 必须 remount 第二次才生效**
  ——首次 bind 会忽略 `MS_RDONLY`，这是 `mount(2)` 的既定行为；漏了这步
  `:ro` 会静默变成可写，那比不支持只读更糟。V.2 专盯这条。
- 宿主路径**必须已存在**，不自动创建：拼错的路径会变成一个空目录，用户直到
  发现数据"不见了"才知道挂错了（V.4）。容器内的挂载点则会自动建——它在
  rootfs 里，不是宿主。
- 挂载在 `pivot_root` **之前**完成：切根后旧根 detach，宿主源路径就没了。
- **拒绝 `-v <任意>:/`**（V.3）：挂到容器根等于把隔离作废。这不是防手滑，
  是防"一条命令让沙箱失效"。
- 宿主模式同样支持：虽不换根，但已在独立 mount namespace 里，bind 只对容器可见。

**Windows 原生程序侧**仍不支持且明确报错：AppContainer 无通用路径重定向，
完整 bind/写重定向需要 minifilter 驱动，撞 §2.4 天花板一。

**Windows OCI/Blink 侧存在不装驱动的可行路径，但尚未实现，不能提前放开 `-v`**：

1. `BLINK_OVERLAYS` 只是冒号分隔的候选根，不表达 `host -> guest`，Windows
   盘符还会被误拆；数据面必须走 `VfsMount(source,target,"hostfs",flags)`。
2. 不得递归修改用户目录 ACL。父 wbox 应以 `FILE_FLAG_OPEN_REPARSE_POINT`
   打开并验证 volume 根，通过 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` 只继承该句柄；
   mount manifest 只记录句柄值、guest target、对象类型与 `read_only`，不泄漏
   宿主路径。
3. Win32 hostfs 必须以继承根 HANDLE 为锚做相对打开，逐组件拒绝 reparse
   越界；不能把 volume 加进单一文本 `WBOX_ROOT` allowlist。
4. `VfsDevice.flags` 的 `MS_RDONLY` 要在 open/create、write/pwrite/writev、
   truncate、共享可写 mmap、unlink/rename/link、mkdir、chmod/chown/utime 等
   **所有**修改入口统一返回 `EROFS`；跨 volume rename/link 返回 `EXDEV`。
5. 首版只承诺目录 bind。当前 hostfs 与挂载点均要求目录，而解析器接受文件路径；
   文件 bind 在真正实现前必须明确拒绝，不能把文件悄悄当目录。

验收必须证明 `:rw` 修改实时回到宿主，`:ro` 的每条写通道均失败且宿主元数据
不变；多卷、嵌套目标、`..`、绝对/相对 symlink、junction、dirfd 逃逸、detach、
`--rm`、stop、启动失败与 supervisor 强杀都不能泄漏句柄、状态目录或宿主权限。

**F9.2 端口映射** `[done]`（Linux 宿主，**仅 TCP**；门禁 N2.1–N2.3）。

选了"wbox 自己做用户态转发"这条路：veth 要 `CAP_NET_ADMIN`（rootless 拿不到），
slirp4netns/pasta 要用户先装（与 §2.2「免安装」冲突，只能当可选加速路径）。

**namespace 权限链必须完整处理**：目标 netns 由容器自己的 user namespace
管辖，宿主不能只调用 `setns(net)`；必须先进入目标 `user`，取得其中的 capability，
再进入 `net`。同时 Linux 不允许多线程进程里的单个线程加入 user namespace，
所以常驻连接器线程方案在 CI 中实际失败为宿主端口 `ConnectionRefused`。

当前实现为每条宿主连接派生一个隐藏 relay 子进程：宿主 listener 接受的 socket
作为 relay 的 stdin/stdout；`Command::pre_exec` 在 fork 后的单线程阶段按
`user -> net` 调用 `setns`，relay 随后连接容器的 `127.0.0.1:GUEST` 并双向复制。
guest 服务可能晚于宿主 listener 就绪，连接端做 5 秒有界重试，不把正常启动竞态
暴露为随机失败。修复已由 Linux 与 Wine 两个原生 Ubuntu 后端门禁共同确认。

几处刻意的取舍：

- 绑 **127.0.0.1** 而非 0.0.0.0：一条 `-p` 不应顺手把容器端口暴露到局域网。
- listener 始终留在宿主 netns；只有 relay 子进程进入容器 netns，容器连接绝不
  在宿主 namespace 建立。
- 与 `--allow-network` **冲突时报错**（N2.3）：后者不建 netns、端口本就在宿主上，
  再转发既无意义又会撞端口；静默二选一会让用户误判实际生效的隔离强度。
- `-P`（发布镜像声明的全部端口）**仍未实现**：它要读 config 的 ExposedPorts，
  与显式 `-p` 是两件事，明确报错而不是当作别名。

**能力边界：只覆盖 TCP。** UDP 与 ICMP 这套做不了，README 与 `--help` 都写明了。

**F9.3 镜像构建** `[done]`（Linux 门禁 B.1–B.6；Windows 门禁 WP.18）。
`wbox build -t NAME[:TAG] [-f Dockerfile] <上下文>`，子集为
`FROM/RUN/COPY/ENV/WORKDIR/CMD/ENTRYPOINT`。

- **`RUN` 直接复用运行期的容器路径**（同一 backend、同一套 namespace 与限额），
  所以构建期与运行期隔离强度一致，不存在"构建时能做、运行时不能"的错位。
- Windows 使用 staging rootfs：`RUN` 经 AppContainer + Blink 执行，临时 profile
  SID 只对 staging 有修改权；发布时重新复制内容与 symlink，不能把临时写 ACE
  带进共享镜像缓存。Windows 绝对 symlink 与 Linux 根路径分别解析，最终仍用
  `strip_prefix(source_root)` 拒绝越界。
- **产物与 `pull` 下来的镜像同布局**（`rootfs/` + `manifest.json` +
  `layers.json` + `config.json`），`run`/`images` 无差别对待。本地构建没有
  registry 层信息，`layers.json` 就写空数组——编一个假 digest 只会误导。
- **未实现的指令一律报错**，不静默跳过：跳过会产出一个"看着构建成功、实则
  少做了事"的镜像，比构建失败难查得多（B.4）。
- **两条安全断言**：`COPY` 源不得逃出构建上下文（否则构建成了读取宿主任意
  文件的通道），目标不得用 `..` 逃出 rootfs。
- **分层缓存已实现**（F9.5，门禁 B.5/B.6）：每个改动型步骤（`RUN`/`COPY`）
  执行后落一份 rootfs 快照，键是**链式**的——把上一步的键、本步指令、以及
  `COPY` 的**源文件内容**一起哈希。链式与内容哈希缺一不可：只看指令的话
  改了文件会命中旧层，那是缓存最危险的失效方式（构建"成功"但内容是错的，
  且没有任何报错信号）。重建时找**最长的已缓存前缀**、只恢复一次，
  后续步骤照常执行。
- 仍不做 overlay：overlayfs 在 rootless 下未必可用，`FROM` 走整份 rootfs 复制。

**F9.4 Windows 文件系统写重定向**。受 §2.4 天花板一约束——不装驱动就做不到
Sandboxie 级别的完整性。可行的用户态近似需要先取证，属 `[TODO-PLAN]` 的
Windows 侧工作。

**F9.7 `--user UID[:GID]`** `[done]`（门禁 U.1–U.4）。

实现路线值得写下来，因为它与 docker 的做法不同，而"不同"正是取舍所在。
rootless 下没有 `newuidmap`/`newgidmap`，`/proc/self/uid_map` **只能写一行、
只能映射一个 id**。所以 `--user 1000` 不是"先进容器当 root 再 setuid 1000"
（那需要 0 和 1000 两条映射，无特权时第二条写不进去），而是直接把宿主那唯一的
uid 映射成 1000：`uid_map = "1000 <hostuid> 1"`。默认行为不变——不带 `--user`
时映射成 0，与此前完全一致。

由此带来的差异必须说清，不能让用户按 docker 的直觉去用：

- 容器内**只有这一个 uid 有效**，其余全是 overflow（`nobody`）。`chown` 到别的
  号会 `EINVAL`，`--user` 也因此不能用来"在同一容器里切换多个身份"。
- 进程在新 userns 里仍持有全部 capability（创建者身份使然），所以 `--user 1000`
  **不等于 docker 的降权**：它换的是 id 号，不是权限面。真正的降权要靠
  `--cap-drop`/seccomp，那是尚未做的一格。
- 只接受数字。用户名要查 rootfs 里的 `/etc/passwd`，而 uid_map 必须在
  `pivot_root` **之前**写完——那个时刻容器的 passwd 还不可达。与其做一个只在
  部分场景正确的名字解析，不如直接报错并说明原因。
- 非 Linux 宿主明确拒绝：AppContainer 没有对应语义，静默忽略会让用户以为
  身份已经换了。

**F9.6 重启策略** `[done]`（门禁 R.1–R.4）。`no` / `on-failure[:N]` / `always`。

循环放在 **supervisor 自己**身上，而不是另起守护进程。除了不引入常驻服务
（§2.2「免安装、无服务」），还白得一个正确性质：`wbox stop` 终止的正是
supervisor，**人为停掉的容器不会被自己重新拉起**——不需要维护一个"这次是不是
人为停的"标记，而那种标记恰恰最容易与实际状态不同步。

代价对应说清：**supervisor 崩溃时重启随之失效**。要覆盖那种情况就得有常驻
守护进程，与上面的前提冲突。

其余取舍：退出码 0 视为"活儿干完了"，`on-failure` 不重启；重启间固定 500ms
退避（起手就失败的容器若无间隔会刷爆日志并空转 CPU；不做指数退避是因为重启
的典型诉求是尽快恢复服务，有上限的场景交给 `on-failure:N`）；与 `--rm` 冲突
时报错——两者对"退出"的处置直接矛盾，静默让一方胜出会让用户无从知道实际
生效的是哪个。

## 4.9 [TODO-PLAN] 跨宿主协作交接点

本节是**给另一台宿主上的 agent 看的工作面**。约定很简单：谁的宿主谁验证，
拿不到的机器不硬猜。

无法在本机验证的东西**不写进产品代码**，改写成这里的一个条目：说清背景、
判据、以及"怎样算做完"。这不是甩锅，是本轮反复吃亏后的结论——两次把 CI
弄红都源于我在没有 Windows/wine 的机器上写 Windows 侧脚本，其中一次测的
根本不是我以为的东西（msys 把 guest 路径 `/busybox` 改写成宿主路径，
造出一个纯自伤的假失败）。

```text
TODO-PLAN
├── W1 Windows 侧 stop 的持续门禁              [Windows agent] 已完成
├── W2 F8.4 exec 的 Windows 原生可对齐子集     [Windows agent] 已完成
├── L1 F8.4 exec 的 Linux 侧实现              [Linux agent] 已完成
├── W3 F9.4 Windows 文件系统写重定向取证     [Windows agent] 待认领
├── W4 build 在 Windows 宿主的可行性          [Windows agent] 已完成
└── L2 Wine 象限的 wineprefix 隔离            [Linux agent] 已完成
```

### W1 Windows 侧 `stop` 的持续门禁 `[Windows agent]` `[done]`

`WP.8-WP.12` 已加入 `test-windows-product.ps1` 并在 Windows 实机通过：
detached workload 用专属 PID 文件证明 supervisor、guest、child 三层在 stop
前全部存活；stop 后三个 PID 全部消失，记录转 exited，重复 stop 幂等，未知
名称失败，rm 清理记录。CI 30250676453 已通过。

门禁实现暴露了一个测试编排坑：不能用 PowerShell 的 `2>&1 | Out-String` 捕获
长命 detached 启动输出。supervisor 可能继承 native-command 管道句柄，调用方
会等待 EOF 直到容器退出。门禁改为按短命父 wbox 的进程句柄等待退出，不捕获
该管道；这条约束属于测试基础设施，不改变产品 detach 语义。

### W3 F9.4 Windows 文件系统写重定向的可行性取证 `[Windows agent]`

**背景**。§2.4 把 Sandboxie-Plus 列为 Windows 程序沙箱的参照物，而它的核心
能力——文件/注册表写重定向——是用 **minifilter 驱动**实现的。wbox 明确不装
驱动（§2.3），这是"免安装、不要管理员权限"这一产品前提的直接后果，不是懒得做。

**要取证的是：用户态能逼近到什么程度。** 候选路径（未验证，供挑选）：

1. AppContainer 的 per-package 存储（`%LOCALAPPDATA%\Packages\<pkg>`）能否
   充当"沙箱私有写入区"，以及非 UWP 的普通进程写系统路径时实际会落到哪里。
2. 目录 ACL + 只读授权把宿主敏感路径挡在外面（`acl.rs` 已有基础）。代价是
   **只能拒绝、不能重定向**——程序拿到的是写失败，而不是"看似写成功"。
   对某些工作负载这是可接受的，对另一些则会直接崩，取证时要区分。
3. 注册表侧有无不依赖驱动的虚拟化手段；若没有，如实记为不可达。

**做完的标准**：给出"能做到哪一档"的结论与依据，并**直接改写 §2.4 那一格的
差距描述**。结论允许是"用户态只能拒绝、做不到重定向"——那同样是有价值的结论，
它让 README 不必再对 Sandboxie 含糊其辞。

### W2 F8.4 `exec` 的 Windows 原生可对齐子集 `[Windows agent]` `[done]`

**结论：只能部分对齐，原生目标可实现，OCI/Blink 目标不可可靠实现。**

Windows 原生 exec 派生运行中容器的同一 AppContainer SID，按记录重建
INTERNET_CLIENT capability，并把挂起创建的新进程加入同一命名 Job 后再恢复。
因此隔离身份、网络策略和 Job 资源限额对齐；工作目录从不含秘密的
`exec_context` 重建。wbox 原生模式本就不做文件系统虚拟化，所以文件系统仍是
同一组经 ACL 授权的宿主路径。

环境变量不写入状态文件，避免把 token、密码和调用方秘密落盘，因此当前 exec
只使用最小清洗环境，不继承原 `run -e`。Windows OCI/Blink 还需要重建 rootfs、
镜像 Env 与 guest 工作目录，当前状态记录不足以兑现，CLI 必须明确拒绝，不能
退化为在宿主执行。

并发契约：

1. `meta.json` 的 `running/stopping` 阶段由状态操作锁保护；stop 或主进程自然
   退出先发布 stopping，再清空 Job，后续 exec 一律拒绝。
2. detached 状态可能早于命名 Job 出现；exec/stop 只对
   `ERROR_FILE_NOT_FOUND` 做两秒有界等待，其他 Win32 错误立即暴露。
3. exec 控制器只在“创建挂起进程 -> 加入 Job -> 恢复”期间持有 Job handle，
   随即关闭。supervisor 保持唯一生命周期所有权，WP.17 以强杀 supervisor
   后主 guest 与 exec guest 均消失为证。
4. CLI 接受 `wbox exec NAME COMMAND [ARG...]` 与可选 `--`；COMMAND 开始后
   `-` 开头参数全部原样透传，贴近 Docker/Podman 的基础位置语义。

`WP.13-WP.17` 已在 Windows 实机与 CI 30250676453 通过。

首次 main CI 中 WP.1–WP.17 全部打印 PASS，但 job 仍返回 1：`finally` 为兜底清理
已删除的记录，最后一次 `wbox rm` 的预期非零码残留成整个 PowerShell 脚本的退出
码。门禁现于 finally 末尾显式清零被忽略的清理码；真正的断言失败仍通过 throw
退出，不会被掩盖。

### W4 `build` 在 Windows 宿主的可行性 `[Windows agent]` `[done]`

Windows 已能执行 F9.3 子集。`FROM` 先复制到 staging rootfs，`RUN` 复用
AppContainer + Blink 运行路径并默认授予网络 capability；每一步使用临时容器记录，
满足 NativeBackend 的 Job/停止协议。构建成功后不直接 rename staging，因为其 DACL
含临时 profile SID 的修改 ACE；发布阶段重新创建目录、只复制文件内容与 symlink，
让最终镜像继承干净 DACL。

Windows symlink 复制复用 Blink 的逃逸约束。Linux `/etc/...` 根路径按容器根解析；
已经重写到 staging 内的 `C:\...` 目标按 Windows 绝对路径解析，二者最终都必须
`strip_prefix(source_root)` 成功。

WP.18 在 Windows 真机从 fixture 构建 `COPY + RUN + CMD` 镜像，立即重建必须命中
`CACHED`；运行产物必须同时输出 COPY/RUN 标记，并断言基础镜像未被修改、staging
无残留。本机与 CI 30265370299 均已通过。

### L2 Wine 象限的 `wineprefix` 隔离 `[Linux agent]` `[done]`

**这是本轮做四象限检视时发现的缺口，之前没人记过。**

先把原本就做对的部分说清楚（初稿我写错过一次，核代码后改正）：wbox **没有**
用宿主默认的 `~/.wine`，而是专用目录，所以**与宿主的隔离一直是有的**。

**缺的是容器之间那一层**：所有容器共用同一个 `~/.wbox/wineprefix`，两个容器
先后跑 Windows 程序会互相看到对方对注册表、C 盘布局、已装组件的改动。

**已实现**：每容器一个 prefix，放在**该容器的状态目录内**
（`~/.wbox/run/<name>/wineprefix`）。这个位置是关键设计——容器记录被 `rm`
或前台容器退出时，`purge_dir` 整棵删掉状态目录，prefix 自然跟着走，
**不需要新增清理路径**。

顺带把 `purge_dir` 从"逐个列举已知产物"改成整棵递归删除：它已经列到第八个
文件了，每加一种状态文件就得记得回来补一行，漏掉的后果很隐蔽——`remove_dir`
因非空静默失败，`ps -a` 里挂一条永远清不掉的记录。

代价如实记录：新 prefix 首次运行 wine 要 bootstrap（铺假 C: 盘），**有秒级
开销**。要跨运行复用就给容器起同一个 `--name`，或用 `WINEPREFIX` 显式指定
（后者优先级最高，也是需要跨容器共享时的出口）。

### L1 F8.4 `exec` 的 Linux 侧实现 `[Linux agent]` `[done]`

**已实现**（`wbox exec <NAME> -- <CMD>`），门禁 P.19–P.22。两个坑都是实测才
暴露的，写下来省得再踩：

**坑一：取不到容器内 pid。** 自然想法是用 `cmd.spawn()` 的返回值，但
**`cmd.spawn()` 在容器退出之前根本不返回**——PID namespace 的双 fork 里，中间
进程负责转发退出码、**永不 exec**，而 Rust 的 `Command::spawn()` 要等 CLOEXEC
错误管道读到 EOF 才返回，写端正握在它手里。这个坑很会骗人：短命容器上一切
"看起来正常"，因为你总是在它结束之后才去看文件。

改为从宿主侧观察：中间进程就是 `Command::spawn` fork 出的**直接子进程**，
起一个线程读 `/proc/<self>/task/<self>/children` 即可，不必等 spawn 返回。
（supervisor 此刻没有别的子进程——看门狗是线程不是进程。）

**坑二：只 setns 不 fork 等于没进 PID namespace。** `setns(CLONE_NEWPID)` 与
`unshare` 同理，**对调用者自己不生效、只对其之后创建的子进程生效**；而
`pre_exec` 已经在 `Command::spawn` 的 fork 之后。实测：容器内 `echo $$` 打出
的是宿主大号 pid（19746），netns 却是对的——**"看着进去了其实没进"**。修法是
在 `pre_exec` 里 setns 之后自己再 fork 一次，结构与
`linux_ns::enter_namespaces` 的双 fork 一致。P.19 专盯这条。

**已确认的 namespace 事实**（同一容器进程实测）：`mnt`/`net`/`user` 均为新的；
`pid -> pid:[4026531836]`（**宿主**）而 `pid_for_children -> pid:[4026532296]`
（容器）——所以附着 PID 必须用 `pid_for_children`，用 `ns/pid` 会附到宿主。

附着顺序：`user` 最先（否则后续 setns 因权限不足失败）→ `mnt`/`net` →
`pid_for_children` → fork。

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
- CLI 以 Docker/Podman 的常用基础命令为迁移入口，精确范围以 F1.7 为准；
  未列出的命令和选项不构成兼容承诺。
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
| Windows 原生容器 | active | WN.1-WN.8、WNET.1-WNET.4、WP.1-WP.17 本地与 CI 30250676453 通过；资源超限仍缺行为门禁 |
| OCI pull/cache/config | active | BusyBox 1.36 与 Debian bookworm-slim 实机运行 rc0；失败 pull 后旧 BusyBox 缓存继续运行 rc0，原子交换与回滚另有 G0 失败注入 |
| Windows Linux guest | active | CI 30253571295 全通过；同一 artifact 实机运行 Alpine 3.20 与 Ubuntu 24.04，Ubuntu 的 shell/Bash/APT/架构/隔离/退出码均通过 |
| Windows shell 矩阵 | component-only | 46 pass、0 fail、1 skip；只证明 wbox-linux 组件 |
| Rust 主机逻辑 | G0 complete | 2026-07-27 Windows 本地 249 pass、0 fail、1 个公网测试 ignored |
| Linux 原生后端 | active | 主路径 G3 已覆盖；资源溢出、失败清理和跨后端语义待补 |
| Linux Wine 路径 | active | PE 分派/退出/网络 G3；资源超限行为待补 |
| 后台生命周期管理 | complete | Linux P.6-P.22 与 Windows WP.6-WP.17 在 CI 30250676453 通过；Windows OCI/Blink exec 明确不支持 |

上述数字是该日期的状态快照，不作为门禁配置。真实基线分别以测试 runner、
`tests/known-failures.txt` 和 `.github/workflows/ci.yml` 为准。

Windows Linux guest 的两项阻断均已修复：`BlinkBackend` 在降权前预建
`/dev`、`/proc`；Win32 私有匿名页不再通过只读 rootfs 中的临时文件分配。
WP.3 保留为 required 门禁，后续任何 AppContainer、rootfs 或 Blink 回归都会
直接使 Windows 产品 job 失败。

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
├── Docker/Podman 生命周期兼容补齐 kill/top
└── 文档收敛为 PRD + 技术参考
```

下一里程碑不使用虚构日期，按验收条件推进：

1. `[done]` Linux cgroup v2 改为兄弟 leaf（父级不可写时退回 supervisor/target
   双 leaf），CI 现造委派子树做门禁，已取得实际限额证据。
2. `[active]` 继续补齐 wbox-linux fork 后 fd、socket 和资源失败回滚边界。
3. `[planned]` 决定是否发布新的 rc；要求全部发布门禁通过且 PRD 状态同步。
4. `[done]` Windows stop 与原生 exec 门禁已通过 CI 30250676453；下一步补资源
   超限 workload 行为门禁，并评估 supervisor 控制通道是否值得支持 exec 环境继承。

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
├── test-windows-product       最终双 exe 的 Windows/OCI 产品路径
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
