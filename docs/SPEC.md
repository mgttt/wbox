# SPEC — wbox: Portable Windows 进程容器

> **历史规格声明**：本文是 MVP 阶段的原始规格（基线文档，保留作决策溯源）。
> 交付物结构、CLI 契约与"非目标"已随实现演进（OCI 镜像拉取、wbox-linux
> 运行 Linux ELF、`cli/backend/oci/acl` 模块等），现状以
> `docs/DEVELOPMENT.md`、CHANGELOG 与当前代码为准。

## 0. 背景与定位
目标机：Windows 10/11（或 Server），**未启用硬件虚拟化**（无 VT-x/AMD-V → 无 WSL2、无 Hyper-V 隔离）。要求 portable：单 exe 免安装，默认模式**不需要管理员权限**、不需要启用任何 Windows 可选功能。

定位：不是 Docker 替代品。它是"进程级容器"——把任意 Windows 原生进程关进一个由 **AppContainer（低完整性令牌）+ Job Object（资源限额）** 构成的隔离单元。对标：Sandboxie 的免驱动内核版、Windows Sandbox 的无虚拟化版。

### 隔离原语选型（决策已定）
| 原语 | 用途 | 是否需管理员 | 是否需 VT-x |
|---|---|---|---|
| AppContainer token | 令牌/权限隔离（最小权限、capability 白名单） | 否 | 否 |
| Job Object | CPU/内存/进程数限额，生命周期收割（kill-on-close） | 否 | 否 |
| 低完整性级别 (Low IL) | 文件/注册表/对象访问的天然边界 | 否 | 否 |
| Silo / Windows Server Container | 更强的命名空间隔离 | 是 + Containers 功能 | 否（但破坏 portable，**不采用**，列为可选高级模式） |
| 文件系统 minifilter（Sandboxie 路线） | 真正的 FS 虚拟化 | 是 + 装驱动 | 否（**不采用**，列为路线图） |

结论：MVP = AppContainer + Job Object + Low IL。镜像格式 = 普通目录（overlay 不做，v1 直接指定工作目录）。

## 1. 交付物
单个 Rust crate，workspace 根即 crate 根：
```
wbox/
├── Cargo.toml
├── README.md
└── src/
    ├── main.rs        # CLI 解析与命令分发
    ├── job.rs         # Job Object 封装
    ├── token.rs       # AppContainer 令牌创建
    ├── sandbox.rs     # 进程启动编排（CreateProcessWithToken / attribute list）
    └── error.rs       # 错误类型
```

## 2. CLI 契约
```
wbox run [OPTIONS] -- <CMD> [ARGS...]

OPTIONS:
  --name <NAME>        容器名（AppContainer profile 名），默认 "wbox-<pid>"
  --memory <MB>        内存上限（MB），0 = 不限，默认 0
  --cpu-pct <N>        CPU 权重/硬性百分比上限 1-100（Job Object CPU rate control），默认 0 = 不限
  --max-procs <N>      最大进程数，默认 0 = 不限
  --no-network         不授予网络 capability（默认也不授予；此 flag 为显式声明，预留）
  --allow-network      授予 INTERNET_CLIENT capability（v1 支持）
  --workdir <DIR>      容器工作目录（"镜像根"），默认当前目录
  --keep-profile       退出后保留 AppContainer profile（默认删除）
  --interactive        连接 stdio（默认行为；--detach 预留，v1 可只支持前台）
  -V/--verbose         打印隔离配置摘要
```

行为：
1. 创建/确保 AppContainer profile（`CreateAppContainerProfile`），成功后注册退出时 `DeleteAppContainerProfile`（除非 --keep-profile）。
2. 创建 Job Object：`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 必开；按参数加 `JobObjectProcessMemoryLimit`/`JobObjectCpuRateControlInformation`/`JobObjectBasicLimitInformation(ActiveProcessLimit)`。
3. 派生 AppContainer 令牌（`DeriveAppContainerSidFromAppContainerName` + capability 组），用 `CreateProcessAsUserW`/`CreateProcessWithTokenW` 或 attribute-list (`PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`) 启动子进程（**二选一以实现可编译为准**，优先 attribute-list 路径，因它对当前用户无需 SeAssignPrimaryToken 权限）。完整性级别设 Low。
4. `AssignProcessToJobObject`（若使用 attribute list 需在创建后立即 assign；注意 breakaway 默认禁止）。
5. 等待子进程退出，转发退出码；stdio 直接继承。
6. `--verbose` 时打印：profile 名、SID、Job 限额、capability 列表。

退出码：子进程退出码原样返回；wbox 自身错误：1=参数，2=profile，3=job，4=进程创建。

## 3. 技术约束
- 工具链：Rust stable，target `x86_64-pc-windows-msvc`。
- 依赖：`windows-sys`（feature：Win32_Foundation, Win32_Security, Win32_System_Threading, Win32_System_JobObjects, Win32_System_Kernel, Win32_Security_AppContainer 等按需）+ `anyhow`。不加 tokio，不加 clap（手写参数解析即可，保持二进制小）。
- 所有 unsafe Win32 调用集中封装，函数级 `/// # Safety` 注释。
- 沙箱是 Linux：交付前必须通过 `cargo check --target x86_64-pc-windows-msvc`（若 rustup 缺 target 则 `rustup target add`）。**无法在本机运行测试**，README 必须标注"已在编译期验证，需真机功能测试"。
- 代码注释/文档：中文为主。

## 4. 非目标（v1 不做）
- Linux 镜像 / OCI 镜像兼容（无虚拟化下不可行，文档说明原因）
- 文件系统 overlay / 注册表虚拟化（需驱动，路线图）
- 网络命名空间隔离（Windows 无此原语；防火墙规则需管理员，路线图）
- GUI 交互桌面隔离（AppContainer 自有 winsta 边界已部分覆盖）
