# 全 Rust 化：`wbox-linux` 模拟器与依赖清理

本文记录 wbox 从"Rust 主程序 + vendored C 模拟器"变成**全 Rust 实现**的结果：
架构、已验证到哪一档、剩余缺口，以及被替换掉的东西去哪了。

替代的旧文档：`vendor/blink/WIN32-PORT.md`（随 `vendor/blink` 一并删除）。

## 1. 改了什么

| 项 | 之前 | 现在 |
|---|---|---|
| Linux ELF 模拟器（`wbox-linux.exe`） | `vendor/blink`，约 122k 行 C（含 win32 移植层与 third_party） | `crates/wbox-linux`，纯 Rust，零第三方依赖 |
| OCI registry 的 TLS | `native-tls`：Linux 链接系统 **OpenSSL**（第三方 C 库），Windows 走 schannel | `rustls` + `rustls-rustcrypto`，**纯 Rust** |
| CI 构建 wbox-linux | windows runner 上装 MSYS2 + MinGW-w64 gcc 编 C11+GNU 扩展 | `cargo build -p wbox-linux`，**CI 不再需要任何 C 工具链** |
| 后端模块名 | `backend::blink` / `BlinkBackend` | `backend::emu` / `EmuBackend` |

整个仓库现在没有任何 `.c` / `.cc` / `.cpp` 参与产品构建。仍在仓库里的
非 Rust 文件只有两类，都不链接进产品：

- `tests/guest/*.c`：**guest 侧**测试程序。它们被交叉编译成 Linux ELF，
  作为"被模拟执行的输入"来验证模拟器的 syscall ABI。它们不是库，也不进
  `wbox.exe` / `wbox-linux.exe`；用 C 写是因为要精确控制发出的 syscall。
- `busybox`：预编译的 Linux 静态 ELF 测试夹具，同样是被模拟执行的输入。

## 2. 模拟器架构

```text
main.rs        CLI：解析参数、读 ELF、装配 Machine、转发退出码
  └── machine.rs   Machine = Cpu + Mem + Os，取指-译码-执行主循环
        ├── elf.rs      ELF64 加载（PT_LOAD 映射、ET_DYN 偏置、PT_INTERP）
        ├── stack.rs    初始栈（argc/argv/envp/auxv）
        ├── mem.rs      guest 地址空间（稀疏页表 + 权限）
        ├── cpu.rs      寄存器 / 标志 / XMM
        ├── exec.rs     译码与整数指令执行
        ├── sse.rs      SSE/SSE2（整数向量 + 浮点）
        ├── alu.rs      算术逻辑与精确标志位
        └── syscall/    Linux syscall 模拟 + fd 表 + VFS 路径翻译
```

### 与 blink 的两处关键设计差异

**1）内存模型：稀疏页表，不是 VA 窗口。**
blink 在宿主里 reserve 一整块 VA 窗口（`WBOX_VA_BITS`，默认 1TB），把 guest
线性地址按位映射进去。那套方案快，但依赖宿主的 reserve/commit 语义
（Win32 `MEM_RESERVE` vs mmap `PROT_NONE`），也是原 WIN32-PORT.md 里一半
崩溃的来源。现在改成 `page number -> Page` 的哈希页表：

- 完全平台无关，Windows / Linux 同一套代码，没有 VA 窗口布局问题；
- guest 的 64 位地址空间不需要宿主真的预留同等 VA；
- 越权访问一定命中"页不存在"，而不是踩到宿主自己的内存。

代价是每次访存多一次哈希查找。**当前是纯解释执行，没有 JIT**，性能显著低于
blink（blink 有 x86→x86 JIT）。定位不变：没有 VT-x/WSL2 时仍然能跑。

**2）标志位即时求值，不做延迟标志。**
延迟标志快，但要为每类运算记住操作数与运算种类，是模拟器里最容易出错的地方。
这里每条指令算完就写 CF/ZF/SF/OF/PF/AF，`alu.rs` 对每种宽度都有单测。

### 未实现的指令一律报错，绝不当 NOP

认不出的 opcode 返回 `Exception::Undefined` 并**打印原始字节**：

```text
wbox-linux: fatal: unsupported instruction at 0x5555555567e5: d9 e8 48 89 e5 ...
  guest pid=1 icount=217195
  rip=0x... rax=0x... （完整寄存器转储）
```

照着字节序列对 `objdump` 就能定位是哪条指令。静默跳过会让 guest 在离故障点
很远的地方以无法追查的方式坏掉——这条是硬规则，syscall 层同理
（没实现的 syscall 返回 `-ENOSYS`，不假装成功）。

## 3. 已验证到哪一档

门禁：`cargo test -p wbox-linux`（142 项：67 单测 + 61 指令语义 + 14 端到端）。
指令语义测试用手工汇编的字节序列，**不依赖任何工具链**，Windows CI 上跑的是
同一批断言。

实测通过（Linux 宿主，`cargo test` 与手工验证）：

| 场景 | 状态 |
|---|---|
| 手写汇编静态 ELF（raw syscall、退出码） | ✅ |
| **静态 glibc** C 程序（printf/malloc/strcpy/strcat/strcmp/循环） | ✅ |
| **动态链接** glibc 程序（PT_INTERP → ld.so → 重定位 → TLS） | ✅ |
| 仓库内静态 busybox：echo/uname/cat/ls/wc/stat/find/sort/head/printf/date/sha256sum | ✅ |
| busybox `sh -c`（内建命令；不含需要 fork 的管道） | ✅ |
| 真实动态 coreutils：cat/id/basename/wc/sha256sum/sort/tr/head/date | ✅ |
| **OCI 镜像端到端**：`wbox image pull alpine:3.20` 后跑其中的动态 musl PIE busybox | ✅ |
| 文件读写往返（openat/read/write/lseek/fstat，落盘内容宿主可见） | ✅ |
| `WBOX_PREFIX` rootfs 约束（`../` 逃逸被挡住，有专项断言） | ✅ |
| 内部控制键（`WBOX_*` / `BLINK_*`）不透传给 guest | ✅ |
| 指令预算（`WBOX_MAX_INSNS`）能打断死循环 guest | ✅ |
| `#!` 脚本经解释器执行 | ✅ |
| `cargo check --target x86_64-pc-windows-msvc` | ✅ |

sha256sum 对已知常量（`"abc"` 的 SHA-256）逐位相符，是整数/移位/向量路径正确性
的一个强信号。

## 4. 剩余缺口

这些是**尚未实现**、会明确报错而不是静默跑错的部分。按影响面排序：

1. **`fork`/`clone`/`execve`/`wait4`（多进程）** —— 返回 `ENOSYS`。
   影响 shell 的管道、命令替换、后台任务。blink 用"快照式 fork"实现过这一档，
   Rust 侧的稀疏页表反而更容易做写时复制，但还没做。
2. **x87 浮点**（`fld1`/`fnstcw`/…）—— 报 `Undefined`。
   SSE/SSE2 的标量与打包浮点已实现（add/sub/mul/div/min/max/sqrt/比较/各类
   int↔float 转换），但 glibc 的 `long double` 路径走 x87。
   已知受影响：`seq`、`printf "%f"`。
3. **线程**（`clone(CLONE_THREAD)`）与真正的 futex 等待。
   当前 `futex` 直接返回成功——单线程下无竞争路径不会走到那里。
4. **信号投递**。`sigaction`/`sigprocmask` 登记接口返回成功但信号永不投递，
   等价于"没有信号发生"。
5. **socket 族与 epoll/eventfd/timerfd/signalfd**。
6. **JIT**。当前纯解释执行，见 §2 的性能说明。
7. `MAP_SHARED` 文件映射的写回（当前是快照式映射）、pty。
8. 宿主 symlink 不防护——rootfs 里若有指向外部的符号链接，guest 能顺着出去。
   与 blink 的限制相同（不是新增风险）。
   **越根路径已收紧**：`/..`、`../../..`、绝对宿主路径一律拒绝（`EACCES`），
   不再"夹到根"后成功。内核语义是夹住，夹住也逃不出 rootfs，但本仓的安全
   审计（`tests/guest/t_sec_path.c`）要求更严的一档。

### guest C 套件的现状

`tests/run-guest-tests.sh` 现在按**容器语义**跑（`WBOX_PREFIX` 指向 workdir）
——这套用例本就是这么设计的，见 `tests/KNOWN-FAILURES.md` 的说明。

当前 **4 通过 / 17 失败**（旧引擎除 `t_net_sockopt@wine` 外全通）。
这是一次真实的 ABI 覆盖回退，逐条根因与分组见
`tests/known-failures.txt`；门禁靠基线判定，新回归照样变红。
安全相关的 `t_sec_path_abshost` 与 `t_sec_path_relesc` **全通且不在基线内**。

## 5. TLS 依赖的取舍（需要复核）

为满足"不引第三方 C/C++"，TLS 换成 `rustls` + `rustls-rustcrypto`。
rustls 的**默认** provider（`aws-lc-rs`、`ring`）都带 C 与汇编源码，
所以只能走 `rustls-no-provider` + 纯 Rust provider 这条路。

**需要注意**：`rustls-rustcrypto` 当前版本是 `0.0.2-alpha`，其 README 明确
说尚未经过安全审计。这是"纯 Rust"与"密码学实现成熟度"之间的真实取舍：

- 现状（本次改动）：整棵依赖树无 C，密码学实现是 alpha 阶段的 crate。
- 备选：回到 `native-tls`（Linux 链接系统 OpenSSL），成熟度高但引入 C 库。

受影响面仅限 `wbox image pull/push` 的 registry HTTPS，不涉及容器隔离本身。
已验证 `wbox image pull alpine:3.20` 走这条链路成功（匿名 token 认证 +
blob 下载 + sha256 校验 + 解包）。若判断成熟度优先级高于"纯 Rust"，
改回 `native-tls` 只需动 `Cargo.toml` 与 `src/oci/registry.rs::new()` 一处。

## 6. 运行期开关

| 开关 | 作用 |
|---|---|
| `WBOX_PREFIX=<目录>` | guest 的 `/` 映射到的宿主目录（兼容名 `BLINK_PREFIX`） |
| `WBOX_STRACE=1` | 打印每次 syscall |
| `WBOX_TRACE=1` | 打印每条指令的寄存器状态（极慢，只用于定位） |
| `WBOX_MAX_INSNS=N` | 指令数上限，超出按 SIGXCPU 终止（0 = 不限） |
| `wbox-linux --version` | 版本号 |

不设 `WBOX_PREFIX` 时是**直通模式**：guest 的 `/` 就是宿主 `/`，工作目录继承
宿主的，行为像一个普通进程。设了就是容器语义：guest 从自己的根开始，
且路径规范化保证 `../` 不能逃出 prefix（有专项测试）。

## 7. 问题上报

请附：① `wbox-linux --version`；② 完整的 `fatal:` 段（含指令字节与寄存器
转储）；③ 最小复现命令与 rootfs 来源；④ 宿主环境。
未实现指令的报告直接贴 §2 里那段字节序列即可。
