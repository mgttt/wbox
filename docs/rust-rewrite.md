# 全 Rust 化：`wbox-linux` 模拟器与依赖清理

本文记录 wbox 从"Rust 主程序 + vendored C 模拟器"变成**全 Rust 实现**的结果：
架构、已验证到哪一档、剩余缺口，以及被替换掉的东西去哪了。

替代的旧文档：`vendor/blink/WIN32-PORT.md`（随 `vendor/blink` 一并删除）。

## 1. 改了什么

| 项 | 之前 | 现在 |
|---|---|---|
| Linux ELF 模拟器（`wbox-linux.exe`） | `vendor/blink`，约 122k 行 C（含 win32 移植层与 third_party） | `crates/wbox-linux`，纯 Rust，零第三方依赖 |
| CI 构建 wbox-linux | windows runner 上装 MSYS2 + MinGW-w64 gcc 编 C11+GNU 扩展 | `cargo build -p wbox-linux`，**CI 不再需要任何 C 工具链** |
| 后端模块名 | `backend::blink` / `BlinkBackend` | `backend::emu` / `EmuBackend` |
| JSON / SHA-256 / Base64 / gzip / tar | `serde_json`、`sha2`、`base64`、`flate2`、`tar` 五个第三方 crate | `crates/wbox-codec`，**第一方、零依赖** |
| 错误上下文链 | `anyhow` | `src/fault.rs`，约 120 行 |
| HTTP 客户端 | `ureq` | `crates/wbox-http`，第一方 |
| TLS | `native-tls`(OpenSSL) → `rustls` + `rustls-rustcrypto` | `crates/wbox-tls`，第一方 TLS 1.3 |

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

门禁：`cargo test -p wbox-linux`（173 项：90 单测 + 61 指令语义 + 22 端到端）。
指令语义测试用手工汇编的字节序列，**不依赖任何工具链**，Windows CI 上跑的是
同一批断言。

实测通过（Linux 宿主，`cargo test` 与手工验证）：

| 场景 | 状态 |
|---|---|
| 手写汇编静态 ELF（raw syscall、退出码） | ✅ |
| **静态 glibc** C 程序（printf/malloc/strcpy/strcat/strcmp/循环） | ✅ |
| **动态链接** glibc 程序（PT_INTERP → ld.so → 重定位 → TLS） | ✅ |
| 仓库内静态 busybox：echo/uname/cat/ls/wc/stat/find/sort/head/printf/date/sha256sum | ✅ |
| busybox `sh -c`：`a && b`、`a; b`、`$(...)`、`a \| b`、`for` 循环 | ✅ |
| 合成的 `/dev/{null,zero,full,random,urandom,tty}`（rootfs 里没有 `/dev`） | ✅ |
| `readlink /proc/self/exe` 给出真实 guest 路径（re-exec 可用） | ✅ |
| 真实动态 coreutils：cat/id/basename/wc/sha256sum/sort/tr/head/date | ✅ |
| **OCI 镜像端到端**：`wbox image pull alpine:3.20` 后跑其中的动态 musl PIE busybox | ✅ |
| 文件读写往返（openat/read/write/lseek/fstat，落盘内容宿主可见） | ✅ |
| `WBOX_PREFIX` rootfs 约束（`../` 逃逸被挡住，有专项断言） | ✅ |
| 内部控制键（`WBOX_*` / `BLINK_*`）不透传给 guest | ✅ |
| 指令预算（`WBOX_MAX_INSNS`）能打断死循环 guest | ✅ |
| `#!` 脚本经解释器执行 | ✅ |
| `-s` / `-e`（被取代引擎的命令行拼写）仍可开 syscall 记录，标签 `(sys)` | ✅ |
| `cargo check --target x86_64-pc-windows-msvc` | ✅ |

sha256sum 对已知常量（`"abc"` 的 SHA-256）逐位相符，是整数/移位/向量路径正确性
的一个强信号。

## 4. 剩余缺口

这些是**尚未实现**、会明确报错而不是静默跑错的部分。按影响面排序：

1. **真正并发的多进程**。`fork`/`vfork`/`clone`/`execve`/`wait4` 已实现，
   走的是和 blink 同一套**快照式 fork**（`syscall/process.rs` 顶部有完整说明）：
   fork 时克隆整个稀疏页表与 CPU 状态，**子进程先完整跑到退出**，父进程再继续，
   `wait4` 从僵尸表取账。

   能跑的是顺序模式：`a && b`、`a; b`、`$(cmd)`、`a | b`（写完再读）、
   `if cmd; then`、`for` 循环。**不能**跑的是两端同时存活的结构——`cmd &`
   之后父子还要互相通信、双向管道、父进程给运行中的子进程发信号。
   这类用法会挂住或读到空数据，不会静默给错答案。

   两个可观察的语义差异，已记进 `tests/known-failures.txt` 的 D 组：
   `waitpid(WNOHANG)` 永远不会报"还在运行"；`kill(child, ...)` 找不到活目标。
2. **`MAP_SHARED` 不跨进程共享，文件映射也不写回宿主。**
   页表在 fork 时按值深拷贝，共享区域因此在父子之间断开。这是
   `t_exec` / `t_fork_mem` 目前唯一的失败来源（A 组）。
   要修就得把页数据从 `Box<[u8; 4096]>` 换成按映射种类区分的共享持有，
   涉及访存热路径，是一个独立的改动。
3. **x87 浮点**（`fld1`/`fnstcw`/…）—— 报 `Undefined`。
   SSE/SSE2 的标量与打包浮点已实现（add/sub/mul/div/min/max/sqrt/比较/各类
   int↔float 转换），但 glibc 的 `long double` 路径走 x87。
   已知受影响：`seq`、`printf "%f"`。
4. **线程**（`clone(CLONE_VM|CLONE_THREAD)`）与真正的 futex 等待。
   `clone` 带线程标志时明确返回 `ENOSYS`——不假装成功，否则 guest 会以为
   多了一个执行流。当前 `futex` 直接返回成功，单线程下无竞争路径不会走到那里。
5. **信号投递**。`sigaction`/`sigprocmask` 登记接口返回成功但信号永不投递，
   等价于"没有信号发生"。因此 `pause()`/`sigsuspend()` 是**确定的死锁**，
   不是"暂时等不到"：返回 `EINTR`/`ENOSYS` 会让 `for (;;) pause();` 满 CPU
   空转（CI 上表现为几百秒后超时），所以直接按 SIGKILL 终止并打印原因。
6. **socket 族与 epoll/eventfd/timerfd/signalfd**。
7. **JIT**。当前纯解释执行，见 §2 的性能说明。
8. **file description 级共享状态**。`dup` 出来的 fd 应与原 fd 共享
   `O_APPEND`/`O_NONBLOCK` 等状态标志与文件偏移；当前 `dup` 走宿主
   `try_clone`（偏移共享），但状态标志各自一份。另含 pty。
9. ~~宿主 symlink 不防护~~ —— **已修**。VFS 换成用户态的
   `RESOLVE_IN_ROOT`：逐段解析，符号链接目标重新从 rootfs 根展开，`..` 与
   绝对目标都作用在"已解析栈"上，栈空即到根，**结构上不可能**指到 rootfs
   之外。不用内核 `openat2(RESOLVE_IN_ROOT)` 是因为它只在 Linux 5.6+，而
   本 crate 也要在 Windows 宿主上跑；一套可移植实现两边共用。
   代价：每段一次 `symlink_metadata`；以及 check-then-use 的 TOCTOU 窗口
   （`openat2` 原子，这里不是）——当前快照式 fork 下父子不并发，窗口很小。
   与 blink 的限制相同（不是新增风险）。
   **越根路径已收紧**：`/..`、`../../..`、绝对宿主路径一律拒绝（`EACCES`），
   不再"夹到根"后成功。内核语义是夹住，夹住也逃不出 rootfs，但本仓的安全
   审计（`tests/guest/t_sec_path.c`）要求更严的一档。

### guest C 套件的现状

`tests/run-guest-tests.sh` 现在按**容器语义**跑（`WBOX_PREFIX` 指向 workdir）
——这套用例本就是这么设计的，见 `tests/KNOWN-FAILURES.md` 的说明。

21 个用例，当前在 Linux 宿主上 **10 通过 / 11 失败**（旧引擎除
`t_net_sockopt@wine` 外全通）。此前是 5 通过 / 16 失败，收紧来自宿主 symlink
防护落地：`t_sec_path` 与 `t_sec_linkabs` 转绿并移出基线（H 组整组清空）。
更早：`t_stress` 已随 `O_TMPFILE` 的实现转绿并从基线移出；`t_exec` 从 7 个失败
降到 2 个、`t_fork_mem` 从 19 个降到 12 个、`t_proc` 从 300s 超时变成快速失败。
这是一次真实的 ABI 覆盖回退，逐条根因与分组见
`tests/known-failures.txt`；门禁靠基线判定，新回归照样变红。
安全相关的四条（`t_sec_path`、`t_sec_linkabs`、`t_sec_path_abshost`、
`t_sec_path_relesc`）**全通且都不在基线内**。

## 5. 第二轮：从"没有 C"收紧到"第一方实现"

`vendor/blink` 删除之后，仓库里确实没有 C 了。但 §2.2.1 的口径随后收紧为
**承载产品能力的实现必须是第一方 Rust**——按这条口径，下面这些第三方 crate
同样要换掉，因为它们承载的正是镜像管理的核心语义：

| 原依赖 | 它承载的产品语义 | 现在 |
|---|---|---|
| `serde_json` | manifest / config / 运行状态记录的解析与序列化 | `wbox_codec::json` |
| `sha2` | blob digest 校验、构建缓存键 | `wbox_codec::sha256` |
| `base64` | registry 的 Basic 认证、PEM 解码 | `wbox_codec::base64` |
| `flate2` + `miniz_oxide` | 层的 gzip 压缩/解压 | `wbox_codec::deflate` |
| `tar` | 层与归档的打包/解包 | `wbox_codec::tar` |
| `anyhow` | 错误上下文链 | `src/fault.rs` |
| `ureq` | registry 的 HTTP 传输、重定向、超时与上限 | `wbox-http` |

### 贯穿这几个模块的一条取舍

**解码方向面对的是外部输入**（registry 给的 manifest、别人压的层、别人打的
tar），所以要完整、要对畸形输入报错、要有资源上界；**编码方向面对的是自己的
输出**，只要合法且对端能读，简单可靠优先。具体落点：

- `deflate` 的**解码器**支持 stored / 固定 / 动态 Huffman 三种块（真实层是
  zlib 压的动态块），**编码器**只做固定 Huffman + 哈希链最长匹配。
- `json` 的解析器有嵌套深度上限（网络输入可能是敌意构造的深层嵌套）。
- `tar` 的读侧吃 GNU 长名/长链接、PAX、ustar prefix、base-256 size；
  写侧走 USTAR + GNU 扩展。
- `wire` 对响应头总量、头条数、响应体字节数三项都设了硬上限。

### 三条不能变的字节级约定

换实现时最容易在不知不觉中改掉、后果又最严重的是这三条：

1. **JSON 对象键按字典序输出**（`serde_json` 默认的 `BTreeMap` 行为）。
   config/manifest 的字节 sha256 之后就是镜像 digest，键序一变 digest 就变，
   本地缓存与已推上去的镜像会对不上。
2. **紧凑 JSON 不含任何空白，非 ASCII 不转义。**
3. **gzip 不写 mtime**，同样输入两次压出来逐字节相同（层可复现）。

### 端到端取证

`wbox pull alpine:3.20` 走完整链路成功：匿名 Bearer token 认证 → 跨主机
重定向到 CDN → 动态 Huffman 解压真实层 → sha256 校验 → tar 解包 → 随后
`wbox run` 起容器执行 busybox。这条路径同时验证了上面六个模块。

## 5.1 TLS：已换成第一方实现

`crates/wbox-tls` 是自实现的 TLS 1.3 客户端。`rustls` +
`rustls-rustcrypto` + `webpki-roots` 已从构建图移除。

### 范围：只做一条路

只支持 **TLS 1.3**、**X25519**、**AES-128-GCM / AES-256-GCM**。
**不做** TLS 1.2 回退、会话恢复（PSK/0-RTT）、客户端证书、吊销检查。

这是**减少攻击面**，不是省事：TLS 的历史漏洞里很大一部分出在版本回退与旧
套件上（FREAK、Logjam、POODLE 都是）。registry 全都支持 1.3，没有回退的
必要；**不实现就不可能被降级到它**。对端不支持 1.3 时明确报错。

### 组成

| 模块 | 内容 | 正确性判据 |
|---|---|---|
| `sha512` | SHA-384/512 + HMAC | FIPS 180-4、RFC 4231 向量 |
| `aes` | AES-128/256 + GHASH + GCM | FIPS 197、NIST SP 800-38D 向量 |
| `x25519` | RFC 7748 密钥协商 | RFC 7748 向量，含 **1000 轮迭代** |
| `bigint` + `rsa` | 大整数、PKCS#1 v1.5 与 PSS 验签 | 用 RFC 8017 测试密钥现签的真实签名 |
| `ec` | ECDSA P-256 / P-384 验签 | 现签的真实签名 + 曲线自检（n·G = ∞）|
| `der` + `x509` + `roots` | 证书解析与链校验、121 张 Mozilla 根 | **121 张真实根全部解析成功，90+ 张自签名验证通过** |
| `kdf` | HKDF + TLS 1.3 密钥调度 | RFC 5869 向量 |
| `record` + `handshake` + `stream` | 记录层、握手、字节管道 | 往返、篡改检测、降级拒绝 |

端到端取证：`wbox pull alpine:3.20` 从 Docker Hub 拉通，manifest digest
与换之前**逐字节一致**；随后 `wbox run` 起容器执行 busybox。

### 安全声明（必须如实说）

**未经第三方安全审计，且不是常量时间实现**（AES 用查表 S-box，大整数运算
不做时序均衡）。换掉的 `rustls-rustcrypto` 当时是 `0.0.2-alpha`、README 同样
写明未经审计——所以这不是"从审计过的换成没审计的"，但**也不构成安全性提升
的理由**。选它的理由是 §2.2.1 第二档的口径，不是安全。

可以接受的三条依据（详见 PRD §2.2.2）：

1. 影响面**仅限 registry HTTPS**，不涉及容器隔离本身；
2. 威胁模型是"从公开 registry 拉镜像"，利用侧信道要与 wbox 争抢同一台机器
   的缓存，而那时攻击者已经能直接读内存；
3. 镜像内容有**独立于 TLS 的完整性保护**——manifest 与每层都按 sha256
   digest 校验，TLS 被攻破也换不掉内容而不被发现。

**不要把 `wbox-tls` 里的密码学实现用到别的地方去。** 它是按 wbox 这一个
威胁模型裁剪的。

### 实现时踩到、值得留下的几处

- **仿射坐标做 ECDSA 会慢到不可用。** 每次点加/倍点都要一次模逆（走费马
  小定理就是一次完整模幂），一次 256 位标量乘约 380 次点运算 = 380 次模幂，
  **实测一次验签超过一分钟**。Jacobian 投影坐标把模逆推迟到最后只做一次：
  降到 644 ms。
- **还是慢，而瓶颈是内存分配不是算术。** `BigUint::rem` 的逐位长除法每一位
  都 `shl`/`add`/`sub` 出一个新的 `BigUint`，512 位被除数就是上千次 `Vec`
  分配。改成原地无分配（算法没变）：76 ms。一条三证书链约 230 ms。
- **老根用 SHA-1 自签，还有根用 P-521。** 第一反应是"那就支持 SHA-1"——错的：
  根的自签名在真实链校验里根本不验，信任来自它在信任库里。正确的分界是
  `SigAlg::Unsupported` / `PublicKey::Unsupported`：**解析不失败、验签必失败**。
- **只测"自产自销的往返"会漏掉一半代码。** `deflate` 的编码器只产生固定
  Huffman，而真实镜像层全是动态块——往返测试一行都走不到动态块解码。
  同理，签名验证只有负向用例的话，"永远返回 false"的实现也能全绿。

## 6. 运行期开关

| 开关 | 作用 |
|---|---|
| `WBOX_PREFIX=<目录>` | guest 的 `/` 映射到的宿主目录（兼容名 `BLINK_PREFIX`） |
| `WBOX_STRACE=1` | 打印每次 syscall |
| `WBOX_TRACE=1` | 打印每条指令的寄存器状态（极慢，只用于定位） |
| `WBOX_MAX_INSNS=N` | 指令数上限，超出按 SIGXCPU 终止（0 = 不限） |
| `wbox-linux --version` | 版本号 |
| `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` | registry 请求的代理（https 走 CONNECT 隧道，TLS 仍是端到端） |
| `SSL_CERT_FILE` | 追加根证书（企业私有 CA）。**只追加不替换**内置根 |

不设 `WBOX_PREFIX` 时是**直通模式**：guest 的 `/` 就是宿主 `/`，工作目录继承
宿主的，行为像一个普通进程。设了就是容器语义：guest 从自己的根开始，
且路径规范化保证 `../` 不能逃出 prefix（有专项测试）。

## 7. 问题上报

请附：① `wbox-linux --version`；② 完整的 `fatal:` 段（含指令字节与寄存器
转储）；③ 最小复现命令与 rootfs 来源；④ 宿主环境。
未实现指令的报告直接贴 §2 里那段字节序列即可。
