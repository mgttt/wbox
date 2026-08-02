# Windows 技术现状与待探索项

> 本文面向外部专家和 agent。内容只记录当前 Windows 工作区已经接入、运行或有测试证据的技术；“规划中”和“待探索”不会冒充已完成能力。代码、测试和 CI 与本文冲突时，以代码和测试为准。

## 1. 当前产品形态

Windows 侧的 wbox 是 Rust 实现的 portable 进程容器，默认不依赖 WSL2、Hyper-V、VT-x、驱动或管理员权限。它包含两条执行路径：

1. 原生 Windows PE/CLI 程序：Windows AppContainer + Job Object。
2. Linux ELF/OCI guest：Windows 宿主进程中运行纯 Rust `wbox-linux.exe` 用户态 guest runtime。

第二条路径不是 WSL2、Docker Desktop 或完整虚拟机：Linux guest 的 CPU、内存和 Linux ABI 由用户态运行时模拟或适配，性能和内核隔离强度不等价。

## 2. 已接入的 Windows 隔离技术

### 2.1 AppContainer

- AppContainer token 和 SID。
- Low Integrity Level。
- AppContainer profile 生命周期管理。
- well-known capability SID 构造。
- 默认拒绝用户目录访问，按需显式授权。
- 无网络 capability 时默认断网。
- 不要求安装驱动或管理员权限。
- 通过 `windows-sys` 使用 Windows ABI 声明，产品逻辑保持在 Rust。

### 2.2 Job Object

- 进程加入命名 Job Object。
- 进程树归属和精确进程控制。
- `KILL_ON_JOB_CLOSE` 风格的整棵进程树回收。
- 内存限制。
- CPU 百分比限制。
- 活跃进程数量限制。
- 进程退出状态和生命周期观察。

### 2.3 Token、句柄和进程控制

- Rust 公共 API 使用 typed process reference、borrowed handle 和 receipt，不把裸 HANDLE 暴露给产品层。
- 精确目标进程 HANDLE 交付、提交和回滚。
- 挂起后恢复的进程创建。
- 子进程继承句柄的显式 allowlist。
- 进程树观察、退出码和强制终止。
- Windows 原生命令行参数和环境块编码，兼容 Windows CRT 参数边界。

## 3. 文件系统、ACL 和路径身份

### 3.1 Private directory ACL

当前 `agenterm-platform::filesystem::protect_private_directory()` 在 Windows 上使用：

- Rust `filesystem-open` 组件级 API，从宿主根目录开始逐级保留父目录句柄。
- Windows `NtCreateFile` 相对句柄打开，避免中间目录组件被静默跟随。
- `FILE_FLAG_BACKUP_SEMANTICS` 打开目录句柄。
- `FILE_FLAG_OPEN_REPARSE_POINT`/等价 reparse 选项不跟随最终或中间 reparse point。
- 已打开对象的 filesystem-entry 分类。
- `READ_CONTROL`/`WRITE_DAC` 只授予最终 ACL 句柄，不向宿主根目录和中间目录传播安全描述符权限。
- `SetEntriesInAclW` 构造当前用户 ACL。
- `SetSecurityInfo` 按已打开 HANDLE 写入 DACL。
- `DACL_SECURITY_INFORMATION` 和 protected DACL。
- 子目录与子文件继承权限。

行为门禁包括：

- 普通文件作为 private directory 时返回 `InvalidInput`。
- junction、symbolic link 和其他 link-like/reparse entry fail-closed。
- junction 目标目录的 ACL 不被修改。
- 新建子目录和文件继承当前用户权限。
- Windows 真机测试覆盖 ACL、junction 和并发读者。

### 3.2 PathLock

- Windows 使用 `CreateMutexW` + `Local\\` named mutex。
- `WaitForSingleObject` 实现阻塞和非阻塞竞争。
- 进程内 reservation 防止同一进程重复取得同一身份。
- 路径绝对化和词法归一化。
- 词法 `..` 归一化有宿主根目录下限，`C:\\..\\x` 不会退化为盘符相对路径；同一规则同时用于 `PathLock` 与 `SlotPermit`。
- `.`、`..`、分隔符和 UNC/长路径前缀处理。
- 已存在路径使用 canonical identity。
- 不存在的中间目录和目标使用 missing-tail identity。
- Windows 大小写不敏感身份折叠。
- `SlotPermit` 复用相同路径身份规则。
- 跨进程、大小写别名、`child/../path` 别名和 missing-tail 别名均有测试。
- Windows 真机还覆盖非 ASCII 文件名的 Unicode 大小写别名竞争，并由跨进程子进程门禁复核。

Unix/Linux/macOS 对应实现使用文件锁和 `flock`，并有目录别名与跨进程门禁；产品层只依赖统一 `PathLock` 契约。

### 3.3 Unix 对齐实现

为了让三宿主的 private-directory 语义接近：

- Linux/macOS 先用 `symlink_metadata` 保持 link-like 输入错误语义。
- 使用 `O_DIRECTORY | O_NOFOLLOW` 打开最终目录。
- 使用已打开 fd 的 `fchmod(0700)` 修改权限。
- 不再对经过检查的 Unix 路径直接调用路径型 `set_permissions`。
- 私有文件使用 `create_new + 0600`。

## 4. Windows IPC 与 broker

- Windows Named Pipe。
- broker 负责宿主与 AppContainer guest 之间的文件系统请求通信。
- pipe endpoint 有 product-neutral typed validation。
- 连接和传输身份经过校验。
- broker 注册请求绑定 Job 和 AppContainer SID，拒绝不匹配的进程。
- IPC 句柄所有权和关闭语义由 Rust 类型封装。

## 5. Windows 上运行 Linux guest

### 5.1 ELF 和 CPU runtime

`wbox-linux` 是纯 Rust Linux guest runtime，当前 Windows 分发包包含 `wbox-linux.exe`：

- ELF64 header 和 program header 解析。
- `PT_LOAD` 映射。
- `ET_DYN` load bias。
- `PT_INTERP` 动态解释器路径。
- 稀疏 guest 页表地址空间。
- x86-64 整数指令和基本控制流。
- SSE/SSE2 整数向量与浮点指令。
- Linux guest ABI 和 syscall dispatch。
- guest 栈、argv、envp、auxv 构造。
- guest `brk`、`mmap`、`mprotect`、`munmap`、`mremap`。
- guest 文件描述符、文件、目录、pipe 和标准设备。
- guest `eventfd`、`timerfd`、`signalfd`、`epoll` 等已实现或接入 Windows fd 层。
- 信号、子进程等待、管道和 shell/ELF 端到端测试。
- VFS prefix/overlay 路径约束。

当前主要验收对象是 x86-64 Linux ELF；32 位 ELF、ARM64 ELF、完整 GUI 和完整 Win32 runtime 仍不是已完成能力。

### 5.2 Windows 映射到 guest 的内存技术

- Windows 文件映射和 paging-file-backed named mapping。
- 共享文件映射和匿名共享映射语义。
- 多进程重新打开命名映射。
- 映射边界、权限和大小校验。
- 映射写回错误传播。
- 64 位文件偏移和大文件映射路径。

## 6. OCI / Docker / Podman 兼容层

Windows 侧的 CLI、OCI 客户端和镜像存储由 Rust workspace 提供：

- OCI Registry HTTP API。
- HTTP/1.1 阻塞客户端。
- TLS 客户端路径。
- manifest、config 和 layer 下载。
- 分层缓存和 rootfs。
- overlay 可写层。
- 镜像 pull/push、build、commit、diff。
- save/load、export/import、cp。
- `run`、`create`、`start`、`stop`、`rm`、`ps`、`logs`、`stats`、`restart`、`pause`。
- `--entrypoint`、`--env-file`、`WORKDIR`、`COPY`、`ADD`、多阶段构建。
- healthcheck、命名 volume、部分 compose。

Windows 运行 Linux 镜像时，Linux guest 主要由 `wbox-linux.exe` 执行；Linux namespace、cgroup、seccomp 等原生 Linux 机制不能直接等价搬到 Windows。

## 7. Rust 自实现和依赖边界

workspace 当前包含：

- `wbox-codec`：JSON、SHA-256、Base64、DEFLATE/gzip、tar 等第一方实现。
- `wbox-http`：阻塞式 HTTP/1.1。
- `wbox-tls`：第一方 TLS 实验实现。
- `wbox-linux`：纯 Rust ELF/CPU/syscall guest runtime。
- `wbox-machine`：ISA、硬件、guest ABI、执行 provider 和三宿主矩阵契约。
- `MachineCore`：将宿主 ABI、guest personality、ISA、provider 和 isolation
  收束为可测试的最小路由值；宿主探测仍由 `agenterm-platform` 提供。
- `wbox-hpc-lab`：CPU、共享内存和并行计算实验。
- `agenterm-platform`：跨项目成立的宿主机制。
- Windows AppContainer capability catalog 已统一覆盖网络、用户库、证书、企业认证、
  可移动存储、日历和联系人 SID；catalog 扩展不会改变 wbox 默认拒绝或自动授予策略。

允许的底层 ABI 声明主要是：

- Windows：`windows-sys`。
- Unix 构建：`libc`，仅用于没有 std 封装的宿主 ABI。

这不等于“不调用操作系统”：wbox 仍然调用 Windows 原生能力，但通过 Rust ABI 声明和边界清晰的 platform adapter 使用。

## 8. 硬件探测与并行实验

`wbox-machine` / `wbox-hpc-lab` 当前在 Windows 使用或探测：

- 当前进程可用并行度和 affinity。
- 系统逻辑 CPU、物理核心、package、processor group。
- NUMA 节点和 SMT 信息读取接口。
- CPU cache hierarchy、cache line 大小和共享宽度。
- page size、allocation granularity、物理内存和可用内存。
- x86-64 ISA、SSE2、AVX2、FMA 能力探测。
- Windows Hypervisor Platform/WHPX 作为 virtualization API candidate 探测；探测结果不会伪称为可用虚拟化后端。
- scoped threads 多核计算。
- child processes 多进程计算。
- cache-line-separated result slots，避免有意 false sharing。
- named shared memory 输入和独立结果槽。
- cold/warm page-touch、读、写、拷贝带宽实验。
- FP64 AVX2 FMA FLOPS microbenchmark。
- 进程 page-fault metrics。

`logical_copies=0` 只表示应用数据没有在进程间重复拷贝，不表示没有内存流量、page fault 或 cache coherence 流量。

## 9. 构建、测试与验证工具

- Cargo workspace 和 Rust 2021。
- Windows PowerShell 产品门禁。
- rustfmt、clippy、`cargo check`、`cargo test`。
- Windows native smoke/product gate。
- Linux guest ELF、Ubuntu 24.04 fixture 和 OCI 集成测试。
- Windows/Linux/macOS 交叉编译矩阵。
- `target/debug/incremental` 和 stale platform revision 清理。
- platform 最小 feature CI：`--no-default-features --features filesystem-conventions`。
- filesystem 最小 feature 测试：`--no-default-features --features filesystem`。
- CI 还检查 `filesystem-conventions` 的 normal dependency tree 不含 `libc` 或
  `windows-sys`，避免轻量 feature 被宿主原生依赖悄然污染。

## 10. 当前明确没有的能力

以下不要被上面的“已接入”清单误解为已完成：

- Sandboxie 级 minifilter 文件写重定向。
- Windows 注册表虚拟化。
- GUI 桌面和窗口隔离。
- 驱动级隔离。
- Windows 上完整 Linux kernel/container namespace。
- GPU/NPU/LPU 实际计算后端。
- DirectX/CUDA/ROCm/Vulkan/Metal 计算 runtime。
- RDMA、SMB Direct 数据通路和 memory registration。
- 真实 NUMA placement、跨 NUMA 内存策略。
- lock-free shared-memory ring 和 scatter/gather I/O 产品后端。
- 完整 Win32 compatibility runtime。
- Linux/macOS 上的第一方 Windows PE runtime。
- ARM64、32 位 guest 的完整执行支持。
- Mach-O runtime 和 WASM machine。

## 11. 建议专家重点咨询的下一批技术项

请专家按“能否抽象为最小 Rust crate、能否在三宿主保持同一契约、是否有真机可验证证据”评估：

1. Windows handle 级目录访问、`NtCreateFile`/`FILE_ID_INFO`、目录 fd/handle capability 和更严格的 component-wise no-follow。
2. Windows Job Object extended limits、processor groups、CPU rate control、memory commit/job accounting 的统一模型。
3. AppContainer capability、broker 安全边界、Named Pipe impersonation 和 endpoint ACL。
4. Linux `openat2`、`RESOLVE_NO_SYMLINKS`、macOS `openat`/sandbox 机制与三宿主路径能力模型。
5. Windows section objects、Unix `memfd`/POSIX shm、共享内存 ring、eventfd/WaitOnAddress 和 Rust 原子内存模型。
6. NUMA 拓扑、processor affinity、Windows processor groups 与 Linux cpuset/cgroup 的可移植抽象。
7. Windows IOCP、Linux io_uring、kqueue、完成端口和 guest async/event ABI 的边界。
8. ETW、Windows performance counters、Linux perf、macOS Instruments 的统一观测契约。
9. DirectX 12/DirectML、Vulkan、CUDA/ROCm、Metal、NPU runtime 的 capability/probe/ownership 模型。
10. RDMA verbs、WinOF/WinSock Registered I/O、SMB Direct 与 zero-copy 网络路径的可验证性。
11. PE/Win32 ABI、SEH、TEB/PEB、loader、COM、GDI/USER32 与第一方 Rust runtime 的分阶段边界。
12. WASM/WASI 与 `wasm-machine`：guest ABI、linear memory、component model、JIT/AOT 和设备能力模型。
13. x86-32、ARM32、ARM64、RISC-V、Xtensa/ESP32 的 artifact identity、ISA feature 和 firmware ABI。
14. QEMU/TinyEMU/v86 类 CPU/MMU/设备模型中哪些能力值得以 Rust crate 形式独立抽象。

专家反馈请分别标记：

- `already-used`：当前代码已有真实使用和测试证据。
- `probe-only`：已有探测或矩阵，但没有可用后端。
- `next-crate`：适合下沉到 `agenterm-platform` 或独立基础设施 crate。
- `wbox-only`：属于 guest ABI、产品路由、OCI 语义或验收策略，不应下沉。
- `not-yet-feasible`：需要驱动、内核、硬件或外部运行时，必须先定义证据门禁。
