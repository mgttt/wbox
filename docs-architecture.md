# wbox 架构设计 — 无硬件虚拟化环境下的 Portable Windows 容器

> 版本：v3 ｜ 日期：2026-07-23
> **实现状态总览**（防止"文档承诺 ≠ 代码现状"）：
> - ✅ 已实现：§1–§5 Windows 进程容器（AppContainer+Job）、OCI 镜像拉取（`wbox image pull/list`，§9.2 前端部分）
> - 🔨 进行中：§9 Linux 后端（BlinkBackend Win32 移植，win32-port 分支）、Backend trait 骨架
> - 📐 纯设计未实现：§6 路线图各项、§8.4 v2/v3 层、§9.2 的 Wsl1Backend/PicoBackend

## 1. 问题定义

目标主机：Windows，**未开启硬件虚拟化（无 VT-x/AMD-V）**，因此：
- WSL2 不可用（依赖 Hyper-V 平台）
- Docker Desktop / Windows Sandbox / Hyper-V 隔离容器 全部不可用
- 且要求 **portable**：单 exe 免安装、默认不需要管理员、不需要启用任何 Windows 可选功能

## 2. 核心决策：隔离技术选型

| 方案 | 隔离强度 | 需要 VT-x | 需要管理员/功能开关 | portable | 结论 |
|---|---|---|---|---|---|
| Hyper-V 隔离容器 | ★★★★★ | ✅ | ✅ | ❌ | 直接排除（硬约束） |
| WSL2 | ★★★★ | ✅ | ✅ | ❌ | 排除 |
| Windows Server 容器（Silo，进程隔离） | ★★★★ | ❌ | ✅（Containers 功能 + 管理员 + 匹配的内核基镜像） | ❌ | 排除，列为 v2 可选高级模式 |
| Sandboxie 式 minifilter 驱动 | ★★★☆ | ❌ | ✅（装驱动） | ❌ | 排除，列为路线图 |
| QEMU/CPU 模拟跑 Linux | ★★★★ | ❌ | 视情况 | 勉强 | 性能差（无 WHPX 加速时纯 TCG 解释执行），不适合主路线 |
| **AppContainer + Job Object + Low IL** | ★★★ | ❌ | **❌（Win8+ 内置）** | **✅ 单 exe** | **✅ 选定** |

**结论**：MVP 用 Windows 内置的原语组合出"进程级容器"：
- **AppContainer**（`CreateAppContainerProfile` + `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`）：派生最小权限令牌，capability 白名单制（默认无任何 capability，即默认断网；`--allow-network` 授予 INTERNET_CLIENT）。内核强制派生令牌完整性级别为 Low → 文件/注册表/命名对象访问被天然限制在 Low IL 可写区域。
- **Job Object**：资源限额（内存 `ProcessMemoryLimit`、CPU `CpuRateControlHardCap`、进程数 `ActiveProcessLimit`）+ `KILL_ON_JOB_CLOSE` 生命周期收割。
- 启动顺序：`CREATE_SUSPENDED` 创建 → 入 Job → `ResumeThread`，消除入 Job 前的逃逸窗口；assign 失败则 `TerminateProcess` 兜底。

**为什么不选 CreateProcessAsUser/WithToken**：需要 `SeAssignPrimaryToken` 特权（通常只有 SYSTEM/服务有），违背非管理员 portable 定位。attribute-list 路径由内核为当前用户派生 AppContainer 子令牌，普通用户可用。

## 3. 系统架构

```
wbox.exe (单文件, 无依赖)
├── main.rs      CLI 解析（手写，无 clap）→ 退出码约定(1参数/2profile/3job/4spawn)
├── token.rs     AppContainer profile 生命周期(RAII)
│                ├─ CreateAppContainerProfile（已存在→回退 DeriveAppContainerSid）
│                ├─ capability SID（CreateWellKnownSid: InternetClient 等）
│                └─ 退出时 DeleteAppContainerProfile（--keep-profile 除外）
├── job.rs       Job Object(RAII)：KILL_ON_JOB_CLOSE 常开 + 按需限额
├── sandbox.rs   启动编排：SECURITY_CAPABILITIES → attribute list
│                → CreateProcessW(SUSPENDED) → AssignToJob → Resume
│                → WaitForSingleObject → 退出码原样转发
└── error.rs     错误分类 → 退出码
```

## 4. "镜像"模型

v1 采用**目录即镜像**：`--workdir <DIR>` 指定应用根目录作为 cwd。无 overlay、无分层。这是 portable 约束下的有意取舍——真正的 FS 虚拟化需要 minifilter 驱动（破坏 portable）。

## 5. 隔离边界与诚实声明

能做到：权限最小化（无 capability 即近似断网 + 无用户私密文件写权限）、资源限额、进程树整体收割、Low IL 文件/注册表边界。
做不到（v1）：FS/注册表视图虚拟化、网络命名空间（Windows 无此原语）、内核级强隔离（无虚拟化时物理上不可能达到 VM 级隔离）。

## 6. 路线图

1. **v2 高级模式（需管理员）**：Silo/Host Compute Service (HCS) 调用 Windows Server 容器能力——仍是进程隔离、不需要 VT-x，但需要启用 Containers 功能。
2. **FS 虚拟化**：可选 minifilter 驱动组件（Sandboxie 路线），与 portable 核心解耦为插件。
3. **网络控制**：WFP（Windows Filtering Platform）按 AppContainer SID 做出站规则（需管理员一次性安装规则）。
4. **OCI 镜像导入（Windows 层）**：解析 OCI layout，仅支持 Windows base layer。
5. ~~CPU 模拟后端（QEMU-TCG）~~：**已被 §9.1 否决**（syscall 直通模型 + GPLv2 + 性能无优势），Linux 支持统一走 §9 的 blink 路线。

## 7. 与同类对比

| | 需要 VT-x | 需要管理员 | 隔离强度 | 镜像生态 |
|---|---|---|---|---|
| **wbox** | ❌ | ❌ | 中（进程级） | 目录即镜像 |
| Windows Sandbox | ✅ | ✅ | 高（VM） | 无 |
| Docker Desktop | ✅ | ✅ | 高 | OCI |
| Sandboxie | ❌ | ✅（驱动） | 中高 | 无 |
| Windows Server 容器 | ❌ | ✅ | 中高（Silo） | Windows OCI |

## 8. 附录 A：Sandboxie 内核驱动技术深度解析与 wbox 的借鉴方案

Sandboxie（现开源为 Sandboxie-Plus）正是"无 VT-x 环境 Windows 沙箱"这一命题下最成熟的参照系。wbox v1 没有采用驱动路线（为保 portable 免安装），但其架构直接决定了 wbox v2 增强层的设计。以下是其机制拆解与逐项借鉴决策。

### 8.1 Sandboxie 三组件架构

```
SbieDrv.sys（内核驱动，Ring 0）
  ├─ 文件系统重定向（I/O 路径过滤/重解析）
  ├─ 注册表虚拟化
  ├─ 进程创建监控（hook ZwCreateProcessEx，维护沙箱进程表，Hash Map O(1)）
  ├─ 令牌手术：用约 6 个未文档化内核符号把沙箱进程令牌改造成
  │   "Untrusted 完整性 + 剥离特权 + 所有组标记 deny-only"
  └─ ObRegisterCallbacks：过滤对象创建/句柄复制（防沙箱进程操作外部进程）
SbieSvc.exe（宿主侧 broker 服务）
  └─ 沙箱内进程经 LPC 发起特权请求 → 校验安全策略 → 代行或拒绝
SbieDll.dll（注入每个沙箱进程的用户态 DLL）
  ├─ hook 大量 ntdll.dll syscall → 重定向到 SbieDrv 评估
  ├─ 文件/注册表虚拟化的"合并视图"在用户态拼装（真实系统 ∪ 沙箱 overlay）
  └─ 对 hook 不住的 Win32 API 提供兼容包装或干脆禁用
```

### 8.2 核心安全模型：为什么用户态 hook 在这里反而是安全的

一般安全工程共识是"用户态 hook 不可靠"（恶意代码可 unhook、可直接 syscall）。Sandboxie 的精妙之处在于**双层夹击**：

1. **顺从小路**：进程走被 hook 的 API → SbieDll 转发给 SbieDrv/SbieSvc broker → 按策略放行/重定向（兼容性由此而来）；
2. **对抗小路**：进程 unhook 或直接 syscall → 内核看到的是那个被手术过的废令牌 → 直接 Access Denied。

即 hook 只负责"恢复可用性"，令牌才是安全边界。这与 Chrome Renderer Sandbox 的 broker 模型同构。wbox v1 恰好已具备该模型的**下半层**（AppContainer 令牌 ≈ Sandboxie 的受限令牌，capability 白名单 ≈ deny-only 组的内核原生版），缺的是**上半层**（broker 兼容层）——这正是 wbox 兼容性上限低于 Sandboxie 的根因，也是 v2 的主攻方向。

### 8.3 逐项借鉴决策

| Sandboxie 机制 | 内核依赖 | wbox 决策 |
|---|---|---|
| 受限令牌（Untrusted IL + 特权剥离） | 未文档化内核符号，版本偏移需随 Windows 更新维护（1.13.3 起改为注册表下发偏移表，不再重编驱动） | **不抄实现，抄思想**：用 AppContainer 内核原生机制达到同等效果，零未文档化依赖——这是 wbox 相对 Sandboxie 的维护性优势（Sandboxie 每次 Windows 大版本都要追内核偏移，追不上就降级为 SBIE1207 无隔离模式） |
| SbieDll syscall hook + broker 恢复可用性 | 用户态 | **v2 采纳（可选层）**：wbox-broker 服务 + wbox-inject.dll，复用微软 Detours 或自行 inline hook；有了它 wbox 才能跑"不知道自己被沙箱化"的普通软件 |
| 文件系统重定向 | SbieDrv 内核驱动 | **v3 可选驱动**：正式 minifilter（Filter Manager 注册、altitude 分配）比 Sandboxie 的路径过滤更规范；需 EV 签名 + 管理员安装，作为独立插件包，portable 核心不受影响 |
| 注册表虚拟化 | 驱动 + SbieDll 合并视图 | 同上，并入 v3；v2 可先用用户态 hook 做只读重定向覆盖 80% 场景 |
| 进程表 + Job 收割 | 驱动进程监控 | **v1 已实现等价物**：Job Object 的 KILL_ON_JOB_CLOSE 是内核原生收割，无需进程表 |
| ObRegisterCallbacks 对象过滤 | 驱动回调（官方文档化 API） | v3 驱动顺带获得，成本低收益高 |
| per-box SID（SandboxieLogon，5.57+） | 驱动 | AppContainer profile SID 天然 per-container，**v1 已等价** |

### 8.4 分层路线总结

```
wbox v1（已交付）：纯官方用户态原语 ＝ Sandboxie 安全模型的"令牌层"
   portable，无管理员，隔离边界诚实但兼容性有限（适合 Agent 代码执行、可信软件加固）
wbox v2：+ 用户态 broker/hook 兼容层（SbieDll/SbieSvc 思路）
   仍需零驱动；目标是跑通"不知情的普通桌面软件"
wbox v3：+ 可选 minifilter/ObCallback 驱动插件（SbieDrv 思路）
   放弃部分 portable 换取 FS/注册表虚拟化与对象过滤，达到 Sandboxie 完整能力
```

一句话：**v1 取了 Sandboxie 的安全骨架，v2/v3 按需补它的兼容血肉与虚拟化外皮，且每一步都把"未文档化内核依赖"替换为官方机制。**

## 9. Linux 镜像后端架构（wbox v2 核心）

> 目标：`wbox run ubuntu:24.04 -- bash` 在无 VT-x 的 Windows 上可用，与 Windows 进程容器共用同一 CLI/隔离层。
> 调研依据：`../research/wsl1-internals.md`、`../research/usermode-linux-emulators.md`、`../research/blink-validation.md`

### 9.1 路线决策（基于调研的结论）

用户拍板"参考 WSL1 自研"。调研结论：

| 候选路线 | 性能 | portable | 工程量 | 结论 |
|---|---|---|---|---|
| 真·WSL1 式内核 Pico Provider 驱动 | 近原生 | ❌（签名驱动+管理员，且 Pico 注册是私有 API，需借道开源 lxmonika 绕过） | 框架 1–3 人月；跑 busybox 级 8–15 人月；通用兼容 20–40 人年（微软因此弃坑） | **v3 可选加速层**，不做主体 |
| fork **blink** + 自研 Win32 后端 | 原生 1/10–1/50 | ✅ 纯用户态单 exe | 移植三大点：mmap→VirtualReserve 自管分配器、signal→VEH、termios→Console | **✅ 选定为主体** |
| 移植 qemu-user | 较好（TCG JIT） | ✅ | 其 linux-user 是"syscall 直通宿主 Linux"模型，换 Win32 等于重写 syscall 层，且 GPLv2 | ❌ |
| WSL1 后端（复用系统自带） | 近原生 | ❌（一次性管理员启用功能） | 最小 | 自动检测的**快捷路径**，有则优先用 |

关键先例支撑：flinux（Foreign LINUX）已在 Windows 用户态无驱动跑通过该路线（32 位、2015 停更、GPLv3，只参考不采用）；noah（macOS）两人业余实现 ~200 syscall 能跑 gcc/make——工程量可控。

### 9.2 总体架构

```
wbox.exe
├── 前端：CLI + OCI 镜像管理（registry v2 客户端，pull/list，layer 解包+whiteout）
│         缓存：%USERPROFILE%\.wbox\images\
├── 后端抽象 trait Backend { prepare(rootfs), spawn(cmd, limits) }
│   ├── NativeBackend   (v1 已有：AppContainer+Job，跑 Windows 程序)
│   ├── BlinkBackend    (v2 主体：wbox-linux 运行时，跑 Linux ELF)
│   ├── Wsl1Backend     (检测到 WSL1 已启用则优先：wsl --import rootfs，近原生)
│   └── PicoBackend     (v3 可选：lxmonika 式驱动，近原生，需管理员)
└── wbox-linux 运行时（fork 自 blink，ISC 许可可闭源）
    ├── ELF64 loader（静态 + PT_INTERP 动态，映射 rootfs 内 ld-linux）
    ├── x86-64 解释器（blink 已有，~600 指令；JIT 留作远期优化）
    ├── Linux syscall 翻译层（blink 180+ 条 → 宿主后端）
    │     └── 宿主后端抽象 HostOs：Linux 宿主（开发验证用）/ Win32 原生（目标）
    ├── VFS 层：rootfs 视图 + /mnt 直通（映射 wbox --workdir）
    └── 进程模型：单进程 + clone 线程（futex→WaitOnAddress）；
        真 fork 不支持（Windows 无私有匿名 COW），fork+exec 模式特判优化
```

### 9.3 syscall 支持分级（验收标准）

| 级别 | 范围 | 目标工作负载 |
|---|---|---|
| L1 | ~60–80 syscall（read/write/openat/stat 族/brk/mmap 子集/ioctl 最小集/exit_group…） | busybox 静态、静态编译的 Agent 工具 |
| L2 | ~150–180（clone/futex/epoll/socket 族/signal/rt_sig*） | ubuntu:24.04 base：bash/coreutils/apt --version |
| L3 | +ptrace 子集/strace 级、netlink 部分 | 开发工具链（gcc/make 参照 noah 水位） |

最难五项（按调研排序）：clone/fork、futex（REQUEUE 族）、mmap（Windows section 语义差异，需保留-提交自管分配器）、signal（VEH 翻译 sigframe）、epoll。

### 9.4 与隔离层的关系

Linux 后端跑在 v1 的 AppContainer+Job 容器**之内**：wbox-linux 进程本身就是被沙箱化的 Windows 进程，Linux 客户程序的一切资源访问都经过它再落入 AppContainer 边界——两层防御，且 Job 限额（内存/CPU）对客户程序天然生效。这是相比"直接跑 WSL1"独有的安全卖点。

### 9.5 风险与缓解

1. **解释器性能**（10–50x）：L1/L2 场景定位为轻量工具/Agent 执行，可接受；JIT（blink 已有 JIT 框架，Cygwin 未启用）与 Pico 驱动是两级加速退路。
2. **兼容长尾**（微软弃坑 WSL1 的根因）：用 syscall 分级 + 工作负载白名单收敛范围，不承诺"通用 Linux 兼容"。
3. **fork 缺失**：现代软件多为 fork+exec（特判优化）或 posix_spawn（直接支持）；真依赖 COW fork 语义的字典型程序（如某些 daemon）列入不支持清单。
4. **blink 上游演进**：fork 后按需 cherry-pick；其代码规模小（~221KB），维护面可控。

### 9.6 实测结论回流（2026-07-23 blink 验证，详见 research/blink-validation.md）

**已验证可用**（ubuntu-base 24.04 rootfs 在 Linux 宿主 blink 下）：bash 5.2 / coreutils / dpkg / apt（含 apt-get update 全流程）、动态 glibc pthread（8 线程 futex 压测正确）、VFS+BLINK_PREFIX rootfs jail（guest 看到 Ubuntu 的 `/`，正好承载 OCI rootfs）。

**必须遵守的约束**：
- blink 1.1 起必须 `--enable-vfs` 编译 + 设 `BLINK_PREFIX=<rootfs>`，否则 guest 动态二进制会误命中宿主 glibc（无 fs 隔离）
- **已知上游 bug**：静态 glibc pthread 程序 100% 崩溃（clone 后 PC 异常，稳定复现）——fork 时必须修复或列入不支持清单；静态 musl busybox 正常
- 仓库已改名 `jart/blink`（旧名 blinkenlights 404）

**性能修正**：JIT 9–12x 慢、fork/exec 16x 慢、**纯解释 78x 慢**——§9.5 原先"10–50x"过于乐观，JIT 是必需品而非优化项。

**两个利好**：`jit.h` 已内建 Win64 ABI（JIT 移植成本大幅降低）；guest `execve` 是进程内重建 Machine，不依赖宿主 exec（fork+exec 特判的工程量比预判小）。
