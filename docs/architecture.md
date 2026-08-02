# wbox 技术架构

本文只描述长期有效的实现边界。产品范围和当前进度见 `../PRD.md`。

wbox 是独立基础设施工具箱，Agenterm 是可能消费它的上层终端与控制面。允许的
依赖方向是 `Agenterm -> wbox -> agenterm-platform/宿主 ABI`；wbox 不依赖 Agenterm
应用层。集成面优先使用版本化 CLI/JSON contract，成熟且需要进程内复用的能力再
公开为稳定 Rust API。

开发反馈方向可以相反：wbox 作为高强度下游消费者，把通用宿主机制的失败复现、
契约测试和修复建议反馈给 `agenterm-platform`。上游只接收脱离 wbox 仍成立的进程、
文件、锁、路径和宿主能力；OCI、guest ABI、MachineCore 与产品路由留在 wbox。

## 1. 总体分层

```text
CLI (`src/cli`)
├── run 参数 -> RunSpec
├── image pull/list/show/rm
└── platform -> 三宿主 × 三来宾 × 双 ISA 能力矩阵
        |
机器与产品平台契约 (`crates/wbox-machine`；`src/platform.rs` 兼容重导出)
├── HostOs / GuestOs / Isa（x86-64、AArch64）
├── HardwareCapabilities / CPU feature / 原生加速 API 候选与探测状态
├── GuestAbi / BinaryFormat（Linux syscall/ELF、Windows NT/PE、Darwin/Mach-O）
├── ExecutionProvider / ProviderCapabilities / IsolationModel / Priority
└── available / legacy / planned / research
        |
目标分类与环境构造 (`src/backend`)
├── Windows NativeBackend
├── Windows EmuBackend
├── Linux LinuxNativeBackend
├── Linux legacy Wine 执行器（待第一方 Win32 runtime 替换）
└── macOS adapters（规划）
        |
平台与数据层
├── Win32: token/job/sandbox/acl
├── Linux: user/PID/mount/net namespace + cgroup/rlimit
├── OCI: registry/config/image/cache
└── wbox-linux: `crates/wbox-linux`（纯 Rust x86-64 模拟器）
```

CLI 只负责解析和分派；`RunSpec` 表达后端无关意图；后端负责把网络、限额、
工作目录、环境和命令翻译为宿主机制。不可将某个平台的句柄、路径或 fd 语义
泄漏到公共层。

`crates/wbox-machine` 是 wbox 的产品与机器语义，`src/platform.rs` 仅保留兼容
重导出；两者都不是操作系统 FFI 集合。未来可由
`agenterm-platform` 提供进程树、原子文件和跨进程锁等机制，但产品路线、执行
provider 选择和能力状态仍由 wbox 持有。契约实际按 Host × Guest × ISA 展开为
18 条；未验收的平台组合必须显式 planned 或
research；尤其不能把所有 `not(windows)` 都当成 Linux。

首批实际接入面是宿主身份、处理器事实与轻量文件系统约定：
`wbox-machine::current_host()` 把 `agenterm_platform::platform_kind()` 映射为 wbox
`HostOs`；`hardware` feature 提供 architecture、pointer width、逻辑处理器数与
SSE2/AVX/AVX2/FMA/NEON 事实，wbox 再映射为桌面 guest `Isa` 与 `CpuFeature`。
该 feature 不引入原生依赖，也不声称 WHPX/KVM/HVF 可用；加速 API 探测、guest ABI
和产品路线继续归 wbox。`EmuBackend` 用
`filesystem-conventions` 统一宿主可执行文件后缀与同目录定位。依赖固定到不可变
Git SHA；`filesystem` 承载宿主约定、私有目录 ACL/mode 与私有原子文件发布，
`locking` 承载 OCI pull 提交锁与状态 owner guard。状态 marker、liveness 分类、
产品超时、目录用途和路由仍由 wbox 持有；未启用 process/UI。
`entropy` 只提供 fail-closed 宿主 CSPRNG 字节填充；TLS 是否 panic、broker 错误类型、
guest `EIO` 与 `AT_RANDOM` 生命周期仍由各消费层决定。禁止在消费层再实现
BCrypt/getrandom/arc4random 接线或退化到时间戳/PID 伪随机。
detached reservation token 是 wbox 产品协议：它消费 32-byte 宿主 CSPRNG、编码为
64 位小写十六进制，并在任何状态变更前 fail closed；格式、传递、一次性领取和回滚
不下沉到 platform。restart config、reservation token、meta、READY/ERROR、
exit-code 与 Linux container PID 等小状态文件统一经 platform 私有原子发布；
wbox 在调用前升级旧状态目录的
private contract，但继续拥有文件 schema、生命周期、错误分类和 best-effort 策略。
宿主文件对象身份也由 platform 提供：已打开文件接口跨 rename/hardlink 稳定，路径
便利接口跟随最终 symlink；Windows 映射 volume/file index，Unix 映射 device/inode。
guest mode/umask、stat ABI 与 VFS 仍由 `wbox-linux` 持有。该对象身份不同于 PathLock
对“可不存在路径”做的规范化身份，禁止用其中一个替代另一个。
单宿主 PID 的 graceful/forceful 终止通过轻量 `process-control` feature；完整 process
inventory、Job/process-group guard 与 pipe API 保持关闭。wbox 继续拥有容器进程树
归属、stop/kill 超时和 Windows Job/Linux namespace 收尾策略。
用户主目录也来自零原生依赖的 `filesystem-conventions`：Windows 只读
`USERPROFILE`，Linux/macOS 只读 `HOME`。wbox 的 `paths::root()` 再追加产品专属
`.wbox`，images/run/volumes/buildcache 不各自解释环境变量；目录名和迁移策略不下沉。
workspace 统一持有 `windows-sys` 版本，各 package 只声明自身最小 feature，避免未来
启用通用原生机制时出现两套 Windows ABI 依赖。
OCI 默认架构也必须消费这一宿主身份：Linux 原生 provider 跟随 target ISA，
Windows/macOS 的近期用户态模拟路线保持 `amd64`，不能把宿主 AArch64 误当成
已实现的 AArch64 guest runtime。

计划中的 provider 层分成三段，依赖只能向下：

```text
wbox 产品策略（host/guest 路由、默认值、能力门禁）
    -> execution provider SPI（probe/lifecycle/exec/logs/limits/snapshot）
        -> 第一方 Rust runtime + agenterm-platform/宿主平台 ABI 机制
```

Docker、Podman、Wine、QEMU、VMware、Parallels 等只作为能力和行为参照，绝不作为
可执行 provider。所有 provider 都是仓内第一方 Rust 实现；缺失能力返回结构化
unsupported。允许通过 Rust bindings 调用 Hyper-V/WHPX、KVM、HVF、Apple
Virtualization.framework 等宿主平台 ABI 取得隔离或加速，但不得调用或链接第三方
产品来补能力。这样 portable、系统原生和 VM 后端共用上层状态模型，同时保持
Rust-only 构建与运行边界。

`wbox-machine-lab` 是这层契约的只读实验台：可探测当前硬件事实、展开完整路线、
检查 ELF64/PE32+/Mach-O64 的 guest/ISA 身份并运行矩阵不变量。它不启动 guest，
也不替代产品门禁。32 位处理器单独建模：x86-32/ARM32 预留，ESP32 的
Xtensa32/RISC-V32 × bare-metal/FreeRTOS 形成独立设备矩阵，不混入桌面 3×3×2；
当前尚未桥接的 ELF32/PE32 与 Mach-O universal 明确拒绝，扩展入口分别由
`WM-ARTIFACT-32` / `WM-ARTIFACT-FAT` 跟踪。

CPU 并行与数据移动按正交维度建模：scalar/SIMD/thread/process 允许组合，数据路径
区分 private copy、borrowed shared、named shared mapping、ring 与 scatter/gather。
`wbox-hpc-lab` 把其中已声明路线变成可重复实验；Windows 当前以同一命名共享映射
驱动 scalar、AVX2、多线程和多进程，并以 scalar checksum 作 oracle。这里的
`logical_copies=0` 仅表示初始化后不在应用 buffer/进程之间复制数据，不代表没有
page fault、cache coherence 或内存流量。NUMA/RDMA 必须经过拓扑/adapter 探测和
真实传输门禁，不能从 API 存在推断可用。
实验启动时只从 `wbox-machine::HardwareCapabilities` 生成一次内核能力快照；不得在
lab 内重复调用架构 feature detector。共享层报告事实，AVX2、FMA、NEON 的组合选择
以及 unsafe kernel 进入条件仍由实验层负责。

FLOPS 只由可审计的浮点 kernel 计算。FP64 lab 在 x86-64 每轮执行 8 条独立 AVX2
FMA，在 AArch64 每轮执行 16 条独立 NEON FMA，分别按 `8 × 4 × 2` 与
`16 × 2 × 2` 计为 64 FLOP。理论峰值、FMA 微基准实测值和受内存/分支/同步约束
的业务吞吐必须分别报告；交叉编译通过的 AArch64 内核在取得真机计时前不是性能证据。

GPU、NPU、LPU 与 CPU ISA 正交，由 `HostOs × AcceleratorClass` 九格矩阵表达，
工作负载分别预填为 parallel/tensor/language compute。当前九格全部是 research；
设备发现与宿主 API 归 `agenterm-platform`，内存、队列、调度、隔离和执行 provider
契约归 `wbox-machine`，未取得运行证据前不得转为 available。
宿主页大小、映射 allocation granularity 与物理总量也由 `agenterm-platform`
的 `host-memory` 契约提供；wbox-machine 直接重导出该事实类型。它不包含 cgroup、
Job Object 或进程限额，资源预算和超限策略仍归具体执行 provider。
路径所在卷的总容量、当前用户可用量和 allocation unit 由独立 `storage` feature
提供；`wbox system df` 持有镜像、容器、命名卷、构建缓存的分类、逻辑字节统计和
reclaimable 策略。platform 不认识 OCI 或 wbox 状态树。

点/线/面/体是分布式执行的共同拓扑：资源节点是点，带方向与 transport 的连接是线，
调度/流水线/数据并行/task graph 是面，跨执行域的 placement、协调、一致性和故障
策略组成 compute fabric。`wbox-machine` 的引用校验必须拒绝重复 ID、悬空端点、重复
成员和悬空 domain；预填的 6 点/5 线/2 面/1 fabric 全部是概念骨架，不是本机探测。

浏览器/WASM 是部署 provider，不是第四宿主 OS。受 v86 的方法启发，未来独立
`wasm-machine` 至少要同时覆盖解释器、热区 x-to-WASM 翻译、线性内存分页、设备总线、
块存储、网络、快照和多实例；Browser 与 WASI 的宿主接口分开建模，形成 16 格研究
矩阵。v86 的价值在于解释器 + 热页翻译与完整 PC 设备模型的纵深，不意味着复制其
代码、依赖或仅限 32 位 x86。技术参考：<https://github.com/copy/v86>、
<https://gist.github.com/copy/ecc99bac5ca0101e024525ddaf620731>。

## 2. Windows 后端

### 2.1 原生程序

启动顺序固定：

1. 创建或复用 AppContainer profile，得到 SID。
2. 创建 Job Object，设置 kill-on-close 及请求的资源限额。
3. 使用 `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` 挂起创建进程。
4. 将进程加入 Job；失败则终止挂起进程并回滚。
5. 恢复线程、等待退出并转发退出码。
6. 按 `--keep-profile` 决定是否删除 profile。

attribute-list 路径避免普通用户通常没有的
`SeAssignPrimaryTokenPrivilege`。AppContainer 派生 token 的完整性级别为 Low。
默认不授予 capability；`--allow-network` 添加 `INTERNET_CLIENT`。Windows 也能
构造 Internet/private server capability SID，但 W8 尚缺外部 peer 流量门禁，
因此未暴露对应 CLI。

Job Object 提供：

- `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`。
- Job 进程树总内存上限。
- CPU hard cap。
- active process limit。

`--workdir` 是工作目录，不是文件系统根或 overlay。

### 2.2 Linux ELF/OCI

`EmuBackend` 定位同目录的 `wbox-linux.exe` 或 `WBOX_LINUX` 指定文件，
将 OCI rootfs 作为 guest 前缀，再经相同 AppContainer/Job 启动链执行。
镜像缓存只向 AppContainer 提供读取执行权限；Windows 运行前在容器状态目录
创建完整的私有 rootfs 副本，并只向该 profile 的确定性 SID 授予修改权。
Win32 真机测试同时验证了 RX ACE 的行为：AppContainer 可读文件，但覆盖和新建
均被拒绝；该 ACL 粒度本身不提供 Windows 原生 bind mount 的路径映射。
前台退出随状态登记清理，后台实例保留到 `rm`。全量复制是语义正确的首版，
后续稀疏层必须保持相同生命周期、cache isolation 与 write/rename/delete 契约。
detached 父进程在全局状态操作锁内预留名称，supervisor 只能凭一次性令牌接管；
ACL 遍历不跟随 reparse point，绝对 Linux symlink 也会重写到私有 rootfs 内，
因此状态目录竞态和链接都不能把删除或授权作用到另一运行实例/宿主路径。

detached 记录在 `~/.wbox/run/<name>/` 保存 `meta.json`、owner `lock`、
stdout/stderr 日志与最终 `exit-code`。supervisor 先写退出码、再释放 owner 锁；
`wait` 和 `inspect` 因而读取同一事实源。异常崩溃没有退出码时保持 unknown。

Windows OCI bind volume 的规划入口是 `crates/wbox-linux` 的 guest VFS
（`syscall/fs.rs`）。此前写的是 "Blink VFS `hostfs`" —— 那份 C 实现已随
`vendor/blink` 删除，规划口径不变但落点换了。宿主根目录将由父进程预开并通过
handle list 精确继承，guest VFS 以该 HANDLE 为锚；不能通过递归修改用户 ACL
或扩大 `WBOX_ROOT` 实现。`:ro` 只有在 VFS 每个路径型、fd 型与 mmap 修改入口
都执行只读拒绝后才算成立。

`wbox-linux` 是**第一方纯 Rust 引擎**（不是 blink 的移植；`vendor/blink` 已
整体删除，两者只在"快照式 fork"这一设计取舍上同源，见 `rust-rewrite.md` §2）。
它维护两层 fd：

- 每个 guest `System` 的 Linux fd 编号。
- 进程共享的宿主 VFS/Winsock/Win32 backing。

快照 fork 后两者编号可能不同。syscall 必须先在当前 `System` 的 guest fd 表
解析，之后只使用 host/VFS fd；元数据查找仍使用原 guest fd。不得把未跟踪的
guest 数字直接回退为同号宿主 fd，也不得重复翻译已转换的 fd。

模拟器的完整架构、已验证范围、剩余缺口和运行期开关见
`rust-rewrite.md`。

### 2.3 共享 CPU 核心（规划）

三宿主 × 两类核心工作负载要求异构执行层共享同一套纯 Rust CPU 语义，而不是分别
复制解释器：

```text
第一方 Rust MachineCore
├── x86-64 CPU core（已有，继续解耦）
├── AArch64 CPU core（规划）
├── ELF loader + Linux syscall/VFS personality  -> Linux OCI
└── PE loader + NT/Win32 ABI personality         -> Windows CLI
```

当前 `crates/wbox-linux` 仍把 CPU、ELF 与 Linux syscall 放在同一 crate。抽取时先
冻结 ISA 无关的地址空间、异常、中断/投递点和 task scheduler 接口，再把寄存器与
指令语义留在各 ISA core；不能为了形式拆 crate 而制造双向依赖。Apple Silicon 可先
解释 x86-64 guest，但 AArch64 从 contract revision 3 起已经拥有独立预填路线，OCI
manifest 选择必须如实反映 runtime 支持的 ISA。

#### 2.3.1 当前模块依赖图（W20）

```text
无 Linux guest ABI 的现有叶子
├── cpu.rs       x86-64 registers/flags/XMM/FS/GS/icount
├── alu.rs       -> cpu::Flags 与位宽辅助
└── mem.rs       稀疏地址空间、页权限与 Fault；当前页大小固定 4 KiB

x86-64 core（仍被 personality 反向穿透）
├── exec.rs      -> cpu + alu + mem + machine + sse
│   └── 0F 05 当前直接调用 syscall::dispatch
└── sse.rs       -> cpu + exec decoder/Rm + machine

Linux executable personality
├── elf.rs       -> mem（PT_LOAD/PT_INTERP 与 ELF64 映射）
├── stack.rs     -> elf::Loaded + mem（Linux argc/argv/envp/auxv）
└── proc.rs      -> elf + stack + mem + syscall::Vfs/errno

Linux kernel/ABI personality
├── syscall/mod.rs      -> Machine + cpu ABI registers + mem + fd/signal/time state
├── syscall/fs.rs       -> Linux fd/VFS/pipe/eventfd/timerfd/signalfd
├── syscall/net.rs      -> fs objects + socket/epoll readiness
├── syscall/signal.rs   -> Machine + exact x86-64 Linux signal frame
└── syscall/process.rs  -> Machine + proc；fork 当前克隆 Cpu/Mem/Os 后串行运行子进程

当前聚合点
└── machine.rs = Cpu + Mem + syscall::Os + trace/budget
    ├── Exception 同时混有 ISA fault、Linux exit 与 Linux signal termination
    └── 因 Os 是具体类型，core 不能脱离 Linux personality 编译
```

由这张图得到的首个可执行边界不是“把 `cpu.rs` 移到新 crate”，而是先消除两条反向边：

1. x86 `syscall` 指令返回带 ABI 入口信息的 core trap，不直接调用 Linux dispatcher；
2. 执行循环只拥有 CPU、AddressSpace、指令预算与 core trap，Linux personality 在外层
   处理 syscall、signal delivery、exit 与调度，再决定恢复执行或终止。

`cpu/alu/mem/exec/sse` 可以在上述边界稳定后组成 x86-64 MachineCore；其中 decoder/Rm
必须与 SSE 一起移动，不能让新 core 反向依赖 `wbox-linux`。`elf/proc/stack` 首批继续属于
Linux executable personality，因为 `PT_INTERP`、auxv、shebang、errno 和 VFS 路径策略已经
交织。fd、socket、signal frame、fork/clone/wait、guest pid 与 VFS 全部是 Linux guest ABI，
不得下沉到 `agenterm-platform`；后者只继续提供 entropy、file identity 和未来可独立验证的
宿主对象机制。

W21 定义接口时还必须保留以下事实，不能用空 trait 掩盖：地址空间目前在 fork 时深拷贝，
`MAP_SHARED` 尚无共享 backing；调度器目前串行执行快照子进程；signal frame 是 x86-64 Linux
布局；`Exception::Killed` 和 `Exception::Exit` 是 personality outcome，不是 ISA exception。
在并发 task、共享映射和 AArch64 core 尚未有行为门禁前，这些能力必须继续报告 partial/planned。

## 3. Linux 后端

### 3.1 公共隔离层

Linux 后端使用 rootless user namespace 映射当前 uid/gid，并建立 PID 与
mount namespace。默认新建 network namespace，只启用 loopback；
`--allow-network` 不创建 netns，从而共享宿主网络。

生命周期使用 PID namespace 与父死亡信号组合，目标是 wbox 异常退出后不遗留
后代。宿主禁止 unprivileged user namespace 时必须给出可操作错误，不得裸跑。

### 3.2 两种文件系统模式

- 镜像模式：bind/mount 必要节点后 `pivot_root` 到 OCI rootfs，旧根不可见。
- 宿主程序模式：不换根，`--workdir` 仅改变当前目录。

这一区分是产品契约。不要为了复用镜像逻辑让宿主程序意外失去宿主文件系统。

### 3.3 资源限制

cgroup v2 是 memory、pids 和 CPU 百分比的首选实现。能否使用取决于当前
cgroup 的写权限、控制器下发和 no-internal-process 规则，不能仅凭
`cgroup.controllers` 文件存在来判断。

实机取证已证伪旧布局：

- cgroup 内有进程时启用 `subtree_control` 返回 `EBUSY`。
- 空 cgroup 先启用控制器后，即使用 root 塞入进程也返回 `EIO`。
- 空父级下发控制器给 leaf 时，leaf 中的 `memory.max`、`pids.max` 和
  `cpu.max` 均存在且可写。

因此受限 target 必须是 wbox 所在 supervisor leaf 的兄弟，而不能是其子节点。
取证已确认该布局成立：委派根下发控制器后，兄弟位置的 target 可写 `memory.max`；
且 wbox 被放进 supervisor leaf 后仍退化到 rlimit，证明剩余问题确实出在 wbox
自身布局，而非环境。

核心约束是：写限额的 cgroup，其**父级**必须已 enable `subtree_control`，
而 enable 的前提是该父级没有直接进程。代码按两种策略依次尝试
（`try_cgroup_plan`）：

策略 A（首选，谁都不用挪）——target 建成 `own` 的兄弟：

```
parent/
  ├── own/          wbox 与它的调用者都在这儿，不动
  └── wbox-<pid>/   限额写这里
```

策略 B（parent 不可写时兜底）——在 `own` 内部自建两个 leaf：

```
own/
  ├── wbox-supervisor/   wbox 把自己挪进来
  └── wbox-<pid>/        限额写这里
```

策略 A 有一条硬性禁止：**父级是根 cgroup 时直接放弃**。往
`/sys/fs/cgroup/cgroup.subtree_control` 写是整机范围的改动（会给所有顶层
cgroup 打开该控制器的记账），一个"把自己关进沙箱"的工具没有理由去改宿主的
全局设置，哪怕权限允许。这种情形退回策略 B 或 rlimit。

**策略 B 有一个前提容易被忽略**：`own` 里必须只有 wbox 自己。否则挪走 wbox
之后 `own` 仍有别的进程，enable 依旧 `EBUSY`。典型情形就是从 shell 启动
wbox —— shell 留在同一个 cgroup 里。这一点是被门禁抓出来的：探针里 shell
`exec` 成了 wbox，只剩一个进程，能过；门禁里测试脚本的 shell 还在，就过不了。
策略 A 正是为覆盖这种（更常见的）情形而加的。

任一步失败（挪不动自己、下发不了控制器、写不了 `*.max`）都退回 rlimit 并清理
已创建的目录，而不是硬报错。

改造后已实测生效（`scripts/probe-cgroup2.sh` 取证环境）：

```
wbox: 限额（cgroup v2） = memory.max=16777216
wbox: cgroup（guest） = .../run/supervisor/wbox-2705
wbox: cgroup（wbox 自身） = .../run/supervisor/wbox-supervisor
==> wbox 走了 cgroup v2 首选路径
```

这是该路径**第一次在任何环境下真正执行**。两个细节印证实现符合设计：
一是 wbox 在探针搭好的 leaf 里又自建了一层 supervisor/target，说明算法自足
——只要它待在任一可写 cgroup 里就能自行完成布局，不依赖外部先摆好架子；
二是运行结束后 `wbox-<pid>` 已被删除、`wbox-supervisor` 仍在，正是预期的
清理语义（后者内有 wbox 自身，删不掉）。

**仍未进门禁**：runner 自身没有可委派的 cgroup，这条路径只有取证步骤
（`continue-on-error`）走得到，`test-linux-backend` 主门禁跑的仍是 rlimit
兜底。要变成真门禁，需要在 CI 里把委派子树的搭建做成前置步骤。

改造同时修掉了一个既有缺陷：旧实现只在失败路径删除创建的 cgroup 目录，
成功路径从不清理，即每成功执行一次 `wbox run` 就在宿主留下一个空的
`wbox-<pid>`（cgroup v2 不会自动回收）。该泄漏一直没被观察到，只因成功路径
在任何环境都没执行过——通读代码才发现，测试不会报。现在 guest 退出后按清单
删除。零覆盖的路径不能默认其正确。

取证还要区分权限与规则错误：迁移进程要求对源和目标的共同祖先有写权限，
`cgroup.procs` 的 `EACCES` 本身不能证明 no-internal-process 规则。
`scripts/probe-cgroup2.sh` 只有在确认进程实际迁移成功后才对布局下结论。

`--memory` 在 cgroup 路径下必须同时写 `memory.swap.max=0`。否则两条路径的
含义不同：`RLIMIT_AS` 直接让分配失败，而 `memory.max` 只限**常驻内存**，
超出的页会被换出去，程序照跑不误。门禁实测抓到过这一点——同一条
`--memory 16`，rlimit 路径下 64MB 分配失败，cgroup 路径下却成功（runner 开着
swap）。同一条命令在不同宿主上强度不同，正是一致性要求禁止的。
`memory.swap.max` 不存在（内核未开 swap 记账）时跳过即可，本来就没有 swap
逃逸；存在却写不进去则放弃 cgroup 方案，宁可退回 rlimit，也不要一个名不副实
的"内存上限"。两条策略共用同一个写入函数，避免各写各的日久漂移。

仅当语义等价时允许 rlimit 回退。例如累计 CPU 秒数不等价于 CPU 百分比；
特权进程的 `RLIMIT_NPROC` 也不能可靠限制进程数。不能满足时明确拒绝。

### 3.4 Windows 兼容运行时

目标是第一方 Rust PE loader + Win32 ABI runtime，并复用上述 Linux 隔离层。
当前 PE 交给系统 Wine、`WBOX_WINE`、版本探测和 wineprefix 都是待删除的 legacy，
只用于迁移期防回归，不满足 Rust-only 产品契约。第一方 runtime 达到端到端门禁后
删除外部进程调用；镜像 rootfs 中的 PE 在此之前继续明确拒绝。

## 4. OCI 数据链

```text
引用解析
-> registry manifest/index
-> 平台选择
-> digest 校验
-> config 与 layer 下载
-> 按顺序应用 tar/whiteout
-> 写入 rootfs + manifest/config/layers 元数据
-> 运行时合并 Entrypoint/Cmd/Env/WorkingDir
```

关键不变量：

- 所有按 digest 获取的数据在使用前校验 SHA-256。
- registry 凭证只发往同 host 或明确允许的认证端点。
- 解包路径逐段解析，绝对路径、`..` 和 symlink 越界均不得逃出 rootfs。
- opaque whiteout 在应用本层普通条目前处理。
- Windows 无创建 symlink 权限时可以复制目标内容，但必须承认其不是引用语义。
- 缓存键必须包含 registry，且路径段适配 Windows 文件名限制。

## 5. 环境和命令

环境优先级为：

```text
后端强制值 > 镜像 Env > 允许继承的宿主 Env
```

默认只继承最小白名单。即使指定 `--env-pass-all`，`WBOX_*` 与 `BLINK_*`
内部键也必须剥离，再由后端写入可信值。日志和 `image show` 只做显示脱敏，
不能修改实际传入 guest 的普通镜像变量。

Windows 命令行必须按 `CommandLineToArgvW`/CRT 规则编码，特别处理空参数、
引号前反斜杠和以反斜杠结尾的带空格参数。

## 6. 代码所有权

| 路径 | 职责 |
|---|---|
| `src/cli/` | 解析、帮助文本、子命令分派 |
| `src/backend/` | 目标分类、RunSpec、环境及宿主后端 |
| `src/oci/` | 引用、registry、config、layer 和缓存 |
| `src/token.rs` | AppContainer profile/capability |
| `src/job.rs` | Job Object |
| `src/sandbox.rs` | Windows 进程启动编排 |
| `src/acl.rs` | Windows rootfs ACL |
| `crates/wbox-linux/` | x86-64 Linux 用户态模拟器（纯 Rust） |
| `tests/guest/` | Linux guest 行为回归 |

共享行为应放在既有公共模块；平台 FFI 留在平台模块。不要为一个调用点引入新
抽象，也不要在文档中复制可直接从 CLI 或代码生成的信息。

## 7. 设计红线

- 不因宿主能力不足而静默降低隔离。
- 不将 guest fd、VFS fd、Win32 HANDLE 和 Winsock SOCKET 当作同一编号空间。
- 不用字符串拼接替代 OCI JSON、tar 或路径组件解析。
- 不在子进程已开始执行后才加入资源管理对象。
- 不把网络可达性当作核心逻辑正确性的唯一证明。
- 不在含进程的 cgroup 节点上启用控制器并把受限 target 建为其子节点。
- 不把历史测试数字写入本手册；状态只维护在 `../PRD.md`。
