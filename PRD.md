# wbox Product Requirements Document

> 本文是项目需求、范围和进度的唯一总入口，主要读者是维护代码的 LLM agents。
> 用户用法见 `README.md`，实现原理见 `docs/architecture.md`，验证命令见
> `docs/testing.md`。最后更新：2026-07-28。

## 0. 内容树（导航）

本文按**内容树**组织：每个主题只有一个归属位置，交叉引用一律指向编号而不是
复制内容。四棵主树是 §2.4（四象限对标）、§4 的 F 系列（功能条目）、
§4.9（跨宿主待办队列）、§5–§8（非功能与发布）。编号（`F9.x`、`W*`、`L*`、
`Q1–Q4`、门禁 ID）一经引用就**永不重编**——它们被代码注释、脚本和另一台宿主
上的 agent 大量引用。

```text
PRD
├── 1  Agent 读取协议 —— 读文档的顺序、事实优先级、状态标记含义
├── 2  产品定义
│   ├── 2.1 目标   2.2 核心价值   2.3 非目标
│   ├── 2.2.1 Rust-only 架构硬约束（两档口径 + 三条棘轮）
│   ├── 2.2.2 自实现密码学的安全声明（wbox-tls 的性质与边界）
│   ├── 2.4 对标基线 ★ 四象限成套的五节
│   │   ├── 2.4   有什么     —— Q1/Q2/Q3/Q4 四张状态表
│   │   ├── 2.4.1 怎么跑的   —— 两条隔离链路 × 两种程序格式
│   │   ├── 2.4.2 对标物怎么做的、差在哪、为什么
│   │   ├── 2.4.3 下一步     —— 缺口粒度
│   │   └── 2.4.4 能力路线图 —— 四格拉到同一深度
│   └── 2.5 两条硬天花板（不装驱动 / 无虚拟化）
├── 3  用户与场景（3.1 主要用户、3.2 S1–S4 核心场景）
├── 4  功能需求树 ★
│   ├── 4.0 完成定义与需求追踪（G0–G4 证据级别、追踪矩阵、门禁 ID 索引）
│   ├── F1 CLI 与运行目标分派      F1.1–F1.7（含 F1.7 兼容命令树）
│   ├── F2 Windows 原生进程容器    F2.1–F2.7
│   ├── F3 OCI Distribution 与本地镜像缓存  F3.1–F3.10
│   ├── F4 Windows 上执行 Linux ELF        F4.R0–F4.R8
│   ├── F5 Linux 原生后端          F5.1–F5.8
│   ├── F6 Linux 上执行 Windows CLI        F6.1–F6.5
│   ├── F7 环境与凭证边界          F7.1–F7.5
│   ├── F8 运维型容器生命周期      F8.1–F8.8（含 F8.a–F8.f 设计答复）
│   ├── F9 对标能力补齐            F9.1–F9.39（每条一个小节，按编号升序）
│   └── 4.9 跨宿主协作交接点 ★
│       ├── 4.9.1 [TODO-WINDOW]   W1–W18、R8
│       └── 4.9.2 [TODO-LINUX]    L1–L20、W5（历史编号）
├── 5  非功能需求 N1–N4
├── 6  当前状态（状态快照，不是门禁配置）
├── 7  里程碑与时间线
├── 8  验收与发布（release gate 的 job 清单）
└── 9  需求变更规则
```

按任务找入口：

| 想知道 | 去哪 |
|---|---|
| 某个能力该不该做、落在哪一格 | §2.4 五节成套读，判断法在 §2.4.1 末尾三条 |
| 某条能力现在什么状态、门禁是哪几条 | §4 对应 F 条目；跨象限的看 §2.4 的四张表 |
| 我这台机器该做什么 | §4.9 里与你**能真实验证**的宿主相符的那棵树 |
| 为什么不做某件事 | §2.3 非目标、§2.5 天花板、§2.4.3 的"不做"列 |
| 某个第三方依赖为什么被换掉 | §2.2.1 的两档口径；密码学部分见 §2.2.2 |
| 历史上 Blink（C 引擎）能跑到哪一档 | §4 的 F4 里带"迁移基线"标注的段落——**历史记录，不代表当前能力** |

## 1. Agent 读取协议

开始任务时按以下顺序建立上下文：

0. **新会话先读 `HANDOFF.md`**：那里有"现在到哪了 / 下一步做什么 / 哪些坑别再踩"
   的交接导航，读完再回来看本文的细节。
1. 阅读本文，确认产品边界、功能状态和当前工作。要判断「某个能力该不该做、
   该落在哪一格」，§2.4 的**五节是成套的**：§2.4 说**有什么**、§2.4.1 说 wbox
   **怎么做的**（两条隔离链路 × 两种程序格式）、§2.4.2 说**对标物怎么做的、
   差在哪、为什么**、§2.4.3 说**接下来往哪走**、§2.4.4 是**四格等深度的能力
   路线图**（Q1 的条目多数标着「待 Windows 侧确认」，接手前先验证再实现）。
2. 按任务阅读 `docs/architecture.md`、`docs/testing.md` 或
   `docs/rust-rewrite.md`，不要无差别加载全部历史。
3. 查看 `git status`、近期提交和相关代码。仓库可能有其他 agent 的并行改动，
   不覆盖、不回退不属于当前任务的修改。
4. 实现后运行与改动范围匹配的测试。有可独立交付的进展时提交并推送 `main`；
   冲突由当前 agent 基于双方意图解决。
5. 功能状态发生变化时更新本文；发布历史写入 `CHANGELOG.md`，不要在多份文档
   复制同一组动态数字。
6. **先看 §4.9 的 `[TODO-WINDOW]` / `[TODO-LINUX]`**：进入与**你这台机器
   能真实验证**的宿主相符的队列；验证不了的不要硬写，把背景、判据和完成标准
   写进另一台宿主的树，交给对应 agent。

事实发生冲突时，优先级为：当前代码和可重复测试 > CI 配置 > 本文 >
技术参考 > `CHANGELOG.md` 历史段落。

状态标记：

- `[done]`：实现和要求的验收均已完成。
- `[active]`：主路径已实现，但仍有明确缺口或正在补充验证。
- `[planned]`：认可的后续范围，尚未进入交付。
- `[out]`：非目标，除非产品范围被明确修改。
- `[TODO-WINDOW]`：必须在 Windows 真机完成或最终验收的交接队列，见 §4.9。
- `[TODO-LINUX]`：必须在 Linux/Wine/overlay 环境完成或最终验收的交接队列，
  见 §4.9。

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
5. **纯 Rust**：wbox 的 CLI、沙箱、镜像管理和跨平台执行运行时全部使用 Rust
   实现。仓库与发布物不得编译、链接、打包或运行 C/C++ 实现。

### 2.2.1 Rust-only 架构硬约束

这是一条发布验收条件，不是代码风格偏好。口径**分两档**，第二档是本轮收紧的：

**第一档：不得有 C/C++。**

- 第一方代码和承载产品能力的第三方组件必须是 Rust；不得以 FFI、辅助进程、
  vendored 源码或预编译动态/静态库绕过。
- 允许通过 Rust bindings 调用宿主操作系统提供的 Windows API、Linux syscall
  和系统动态加载器；这些是平台 ABI，不等于引入第三方 C/C++ 实现。
- Rust crate 依赖必须审计其传递依赖和 `build.rs`，不得在构建时编译 C/C++，
  也不得链接项目自带或第三方 native runtime。
- `vendor/blink`（约 122k 行 C）**已删除**，由纯 Rust 执行引擎
  `crates/wbox-linux` 取代。仓库里不再有任何参与产品构建的 C/C++ 源码，
  CI 也不再需要 C 工具链。
- 仍然保留在仓库里的非 Rust 文件只有 `tests/guest/*.c` 与预编译 `busybox`：
  它们是**被模拟执行的 guest 输入**（测试夹具），不是库，也不链接进任何发布物。

**第二档：承载产品能力的实现必须是第一方。**

"没有 C"是必要条件，不是充分条件。`serde_json` 解析 manifest、`sha2` 算
blob digest、`flate2` 解层、`tar` 解包、`ureq`/`rustls` 跑 registry 协议与
TLS——这些都是纯 Rust，但它们承载的是**镜像管理的核心语义**，正确性与
安全性直接决定 pull/push 的行为。这一档要求它们在本仓可读、可测、可改。

**已全部达成。** 替换清单：

| 原依赖 | 现在 |
|---|---|
| `serde_json` / `sha2` / `base64` / `flate2` / `tar` | `crates/wbox-codec`（零依赖）|
| `anyhow` | `src/fault.rs` |
| `ureq` | `crates/wbox-http` |
| `rustls` / `rustls-rustcrypto` / `webpki-roots` | `crates/wbox-tls` |

改完之后，**整棵构建图**（`Cargo.lock` 共 16 条，此前 119 条）只剩：

- 五个第一方 crate：`wbox`、`wbox-codec`、`wbox-http`、`wbox-tls`、`wbox-linux`；
- `libc` / `windows-sys` 及其 target 垫片——**平台 ABI 声明**，只是 extern
  声明，不编译任何第三方实现代码，属第一档明确允许的那类。

三条棘轮盯死这个结果，任何回流都会立刻变红（`src/runtime/mod.rs`）：

1. `native_source_debt_cannot_expand_or_escape_legacy_roots`：有没有 C；
2. `replaced_third_party_crates_cannot_come_back`：被换掉的 crate 有没有
   重新出现在某个 `Cargo.toml` 里；
3. `dependency_graph_is_first_party_plus_platform_abi_only`：直接盯
   **`Cargo.lock`**——直接依赖清白但拖进来一串传递依赖，前两条都看不见。

### 2.2.2 自实现密码学的安全声明

`crates/wbox-tls` 是自实现的 TLS 1.3 客户端。**必须如实说清它的性质**：

- **未经第三方安全审计**，且**不是常量时间实现**（AES 用查表 S-box，
  大整数运算不做时序均衡）。
- 换掉的 `rustls-rustcrypto` 当时是 `0.0.2-alpha`、README 同样写明未经审计，
  所以这不是"从审计过的换成没审计的"，但**也不构成安全性提升的理由**。
  选它的理由是第二档口径，不是安全。

**为什么可以接受**：

- 影响面**仅限 `wbox pull/push` 的 registry HTTPS**，不涉及容器隔离本身。
  隔离由 AppContainer/Job（Q1/Q2）与 namespace/cgroup（Q3）承担，与 TLS 无关。
- 威胁模型是"从公开 registry 拉镜像"。要利用侧信道，攻击者得在同一台机器上
  与 wbox 争抢缓存——那种情形下他已经能直接读 wbox 的内存了。
- 镜像内容有**独立于 TLS 的完整性保护**：manifest 与每一层都按 sha256
  digest 校验，TLS 被攻破也换不掉镜像内容而不被发现。

**收窄攻击面的做法**（都在实现里，不是口号）：只做 TLS 1.3、只做 X25519、
只做 AES-GCM；**不实现 TLS 1.2 回退**（不实现就不可能被降级到它——FREAK、
Logjam、POODLE 都是回退路径上的洞）、不做会话恢复、不做客户端证书。
对端不支持 TLS 1.3 时**明确报错**而不是悄悄退回更弱的协议。

已知不做且如实记录的：**不做吊销检查（CRL/OCSP）**——两者都要额外网络请求，
主流客户端还普遍软失败（查不到就放行，等于没查）。

**资源上界**（自实现的格式栈面对的都是网络输入，每一处都要有上界）：
HTTP 响应体 8 GiB、gzip/DEFLATE **解压产物** 8 GiB（压缩炸弹的防线——
DEFLATE 的理论压缩比超过 1000:1）、单个 tar 条目 4 GiB、JSON 嵌套深度 128、
响应头总量 256 KiB / 条数 200、TLS 记录按 RFC 8446 的 2^14。

完整取舍见 `docs/rust-rewrite.md` §5。

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
`进行中` = 已有组件级门禁但尚未对用户开放；`无` = 未实现；
`不适用` = 这一格的形态下该能力没有意义；`不做` = 撞天花板或属非目标。

#### Q1 Windows 宿主 × Windows 程序 —— 对标 Sandboxie-Plus

| 参照物特征能力 | wbox | 说明 |
|---|---|---|
| 进程隔离与降权 | 有 | AppContainer SID + 低完整性级别；**默认拒绝**访问用户目录，rootfs 要显式授 ACE 才读得到（`acl.rs`）|
| 默认断网 | 有 | 不授 `INTERNET_CLIENT` capability |
| 资源限额（内存/CPU/进程数）| 有 | Job Object；Sandboxie 本身反而不强调这块 |
| 进程树可靠回收 | 有 | Job `KILL_ON_JOB_CLOSE` |
| 生命周期（ps/stop/kill/top/rm/logs/exec/inspect/wait）| 有 | F8 全套，含 F1.7.9 `kill` 与 F1.7.10 `top` |
| **文件系统写重定向（copy-on-write）** | **不做** | Sandboxie 用 minifilter 驱动。W3 真机取证确认 32 位无 manifest 进程也不会触发 VirtualStore；原生 PE 路径没有介入点。可用近似仅是「拒绝 + 显式授权」以及 AppContainer 自动提供的 package 私有 `LOCALAPPDATA`/临时目录 |
| **注册表虚拟化** | **不做** | 同上 |
| 命名沙箱的持久化内容 | 不做 | W3 证实只有 package 私有标准目录可写，且当前 profile 生命周期结束时清理；没有任意路径写重定向可构成 Sandboxie 式持久内容 |
| 强制程序入沙箱（Forced Programs）| 无 | 需要驱动或全局钩子，撞天花板一 |
| GUI 程序沙箱 | 不做 | §2.3 非目标 |
| `--restart` 重启策略 | 有 | 与 Q3 同一实现（循环在 supervisor 内） |
| 卷挂载 `-v` | 不做 | 原生程序走宿主文件系统，本就没有"挂载"这一层；隔离靠 ACL 授权 |
| `--user` / `--cap-*` / seccomp / healthcheck | 不做 | 均为 Linux 原语（uid 映射 / capability / seccomp-bpf / setns 探针），AppContainer 无对应语义，一律明确报错 |
| compose 多服务 | 不做 | 服务间靠共享 network namespace 互通（F9.11），Windows 无对应原语；单服务 compose 文件可用 |
| `diff` / `commit` / `cp` | 不做 | 三者都读 overlay 可写层（F9.12），Windows 侧没有那一层；一律明确报错并说清原因 |
| `stats` | 不做 | 占用数据取自 cgroup / `/proc`，Windows 要另走 Job object 的记账接口，尚未实现（F9.24）|
| `pause` / `unpause` | 不做 | 靠向容器内每个进程发信号实现（F9.21），Windows 进程组语义不同，需另行设计 |
| `export` | 不做 | 同上：要 overlay 可写层才能取到容器文件系统。`import` **不受此限**，它只解 tar 造镜像，两平台一致 |
| `ps -q` / `rm -f` / 多容器名 | 有 | F9.29–F9.30 是纯 CLI 层，无平台原语依赖；`rm -f` 的停止那步走 Windows 侧的 Job 终止路径 |
| `images -q` / `rmi` 多引用 | 有 | F9.31 只碰镜像缓存目录，两平台同一实现 |
| `inspect` 的 `Mounts` / `PortBindings` | 不适用 | F9.33 如实反映 `-v`/`-p`，而这两个选项在 Q1 本就不做（原生程序走宿主文件系统，没有「挂载」这一层），故恒为空是**如实**而非缺陷 |
| `inspect` 的 `State.Paused` | 不适用 | F9.32 从 `/proc` 判暂停；Windows 无 `pause`，恒为 false 是如实的 |
| 命名卷 | 部分 | F9.35 的卷管理（create/ls/rm/inspect）是纯目录操作，两平台一致；但 Q1 不做 `-v`（原生程序走宿主文件系统），所以卷建得出来、挂不进去 |

**这一格是四象限里差距最大的**，且差距的主因不是工作量而是架构前提：
不装驱动就做不到驱动级别的重定向完整性。

#### Q2 Windows 宿主 × Linux 镜像 —— 对标 WSL2 / Docker Desktop

| 参照物特征能力 | wbox | 说明 |
|---|---|---|
| 运行 Linux OCI 镜像 | 部分 | 纯 Rust 引擎已取代 Blink：Alpine 3.20 / Ubuntu 24.04 手工跑通、静态 BusyBox 过 WP 产品门禁。**但 ABI 覆盖仍有明显缺口**（guest C 套件 21 个用例在 Linux 宿主上 14 通过 / 7 失败，基线见 `tests/known-failures.txt`、裁决理由见 `tests/KNOWN-FAILURES.md`），线程/信号/socket/`MAP_SHARED` 未做，Python 等动态运行时尚不可靠 |
| 免虚拟化 | **有，且这是 wbox 存在的理由** | WSL2 要 Hyper-V，wbox 不要 |
| 双层隔离 | 有 | AppContainer 套模拟器 |
| 可写 rootfs 层 | 有 | 运行前把只读镜像缓存复制成容器私有 rootfs，只向该 profile 的确定性 AppContainer SID 授修改权（F4.8 / `acl.rs`）；与 Q3 的 overlay upper 不是同一机制 |
| **接近原生的性能** | **不做** | 用户态解释/JIT，天花板二 |
| 卷挂载 `-v` | 计划重做 | 要由纯 Rust guest VFS + Rust supervisor 实现（`src/broker.rs` 目前只到 HELLO/PING，OPEN 未开放），CLI 继续明确拒绝。历史上的 Blink/C brokerfs 实验已撤回，不作为路线 |
| **解释执行的启动开销** | 已知 | 真实镜像启动约 20–46 秒（纯解释、无 JIT）。这是 §2.4 「天花板二」的具体数字，不是待修缺陷 |
| 端口映射 `-p` | 不做（语义不适用）| **guest 的 socket 就是宿主 socket**：两代引擎都没有自建网络栈，当前的 `crates/wbox-linux` 连 socket 族都还返回 `-ENOSYS`（见 §4.9 W5）。guest 绑的端口即宿主端口，没有可映射的东西 |
| 网络隔离模型 | 部分 | 与 Q3 **不同**：Q3 靠 netns，Q2 靠 AppContainer 不授 `INTERNET_CLIENT`——是能力开关而非独立网络栈 |
| 镜像 push | 有 | F9.13 是纯 Rust 且不带平台 cfg，与 Q3 同一实现 |
| `--cap-*` / seccomp / healthcheck | 不做 | 同 Q1：均为 Linux 原语，明确报错 |
| compose 多服务 | 不做 | 同 Q1：依赖共享 netns |
| 镜像构建 | 部分 | F9.3 子集；Windows `RUN` 经 AppContainer + 模拟器；**分层缓存与 Q3 同一实现**，WP.18 断言二次构建出现 `CACHED`；F9.36 的四条新指令是纯解析与 config 写入，两平台一致 |
| restart policy | 有 | 与 Q3 同一实现：循环在 supervisor 内，不引入常驻服务 |
| `--user UID[:GID]` | 不做 | AppContainer 没有 uid 映射语义，明确报错而非静默忽略 |
| 完整 syscall 覆盖 | 部分 | 缺口见 F4：异步信号语义、glibc pthread/clone、ptrace |
| `diff` / `commit` / `cp` / `stats` / `pause` / `export` | 不做 | 同 Q1：分别依赖 overlay 可写层、cgroup·`/proc`、信号语义，Windows 侧均无对应原语，一律明确报错 |
| `import` | 有 | 只解 tar 造镜像，不碰容器运行期，两平台同一实现 |
| `ps -q` / `rm -f` / 多容器名 / `images -q` | 有 | F9.29–F9.31 是纯 CLI 层与镜像缓存操作，两平台同一实现 |
| 命名卷 | 部分 | 卷管理可用；挂载要等 Q2 的 `-v` 重做（纯 Rust guest VFS），届时命名卷自动适用——它只是给 `-v` 提供一个源 |
| `inspect` 的 `Mounts` / `PortBindings` | 部分 | F9.33 的机制是通用的；但 Q2 的 `-v` 尚未重做（等纯 Rust guest VFS）、`-p` 语义不适用（§4.9 W5），故当前恒为空——**这是上游能力的缺口，不是 inspect 的缺陷** |
| 容器内工作目录 | 计划自动获得 | F9.37 已把 `guest_workdir` 放进 `RunSpec`；Q2 的镜像路径等纯 Rust guest VFS 落地后读它即可，不必再改结构 |
| `save` / `load` | 有 | F9.22 是纯 Rust 且不带平台 cfg，与 Q3 同一实现 |
| systemd / 服务 | 不做 | 非目标 |

#### Q3 Linux 宿主 × Linux 镜像 —— 对标 Podman / Docker

**四象限里最接近对标的一格。**

| 参照物特征能力 | wbox | 说明 |
|---|---|---|
| rootless 运行 | 有 | user/PID/mount/net/**IPC/UTS** namespace（F9.15 补齐后两个）|
| 资源限额 | 有 | cgroup v2 首选，受限时明确回退或拒绝 |
| `run/exec/ps/logs/stop/kill/top/rm/inspect/wait` | 有 | F8 全套；`kill`（F1.7.9）与 `top`（F1.7.10）为后补 |
| `diff` | 有 | F9.19：A/C/D 与 docker 对齐；直接读 overlay upper，不扫全树（门禁 DF.1–DF.3）|
| `commit` | 有 | F9.20：容器改动固化成新镜像，与基础镜像共享磁盘（门禁 CM.1–CM.4）|
| `pause` / `unpause` | 部分 | F9.21：用 SIGSTOP/SIGCONT 而非 cgroup freezer（后者要求设了限额才存在），语义不完全等价（门禁 PZ.1–PZ.3）；暂停状态在 `ps`/`inspect` 可见（F9.32）|
| `save` / `load` | 有 | F9.22：镜像打包成 tar 离线搬运，含原始层故搬过去仍可原样 push（门禁 SL.1–SL.6）|
| `cp` | 有 | F9.23：宿主与容器双向拷贝，走 overlay 分层视图故不必 `setns`、容器已退出也能取（门禁 CP.1–CP.6）|
| `stats` | 有 | F9.24：有专属 cgroup 时读内核记账，没有则按 `/proc` 逐进程累加并**标注来源**（门禁 ST.1–ST.5）|
| `export` / `import` | 有 | F9.25：容器文件系统的**裸** tar（与 `save`/`load` 搬的东西不同，见下）；import 收任意来源归档故只挡穿越、全部落在 `rootfs/` 下（门禁 EX.1–EX.7）|
| `restart` | 有 | F9.26：停掉再按原配置起来；**`run -d` 起的容器现在也记得住启动配置**，故 `start`/`restart` 对它同样可用（门禁 RT.1–RT.7）|
| `rename` / `prune` | 有 | F9.27：改名只对未运行容器开放（名字被用在可写层路径等处）；prune 不加 `-f` 只列清单（门禁 RN.1–RN.6）|
| `logs -f` / `--tail` | 有 | F9.28：跟随到容器退出后自行结束；**认日志截断**，否则超上限后跟随会静默哑掉（门禁 LG.1–LG.4）|
| `ps -q` / `rm -f` | 有 | F9.29：补齐 `wbox rm -f $(wbox ps -aq)` 这条清场惯用法；`-q` 只出名字，`-f` 先停再删（门禁 RMF.1–RMF.5）|
| 多容器名一致性 | 有 | F9.30：`stop`/`pause`/`unpause` 也收多个名字（此前只有它们收一个，而 `kill`/`rm`/`start` 早就收多个）（门禁 RMF.6–RMF.7）|
| `images -q` / `rmi` 多引用 | 有 | F9.31：**并修掉 IMAGE 列印缓存目录名、照抄去 rmi 用不了的缺陷**；补齐 `wbox rmi $(wbox images -q)`（门禁 IMQ.1–IMQ.3）|
| 暂停状态可见 | 有 | F9.32：**修掉 `Paused` 写死 false、`ps` 把暂停容器显示成 running 的缺陷**；状态从 `/proc` 实测（门禁 PZ.4–PZ.5）|
| `inspect` 的挂载与端口 | 有 | F9.33：**修掉 `Mounts` 写死 `[]`、端口根本不出现的缺陷**；`.Mounts`/`.HostConfig.PortBindings` 如实反映 `-v`/`-p`（门禁 INS.1–INS.3）|
| 状态口径与网络模式 | 有 | F9.34：状态标签收敛成一处（`compose ps` 此前漏了 `paused`）；`NetworkMode` 补上 `container:<NAME>` 三态（门禁 INS.4–INS.5）|
| `events` | 不做 | 需要常驻事件流与订阅端，与 §2.2「免安装、无服务」直接冲突；wbox 没有 daemon 可发事件 |
| `update`（改运行中容器的限额）| 不做 | 限额只在设了 `--memory`/`--cpu-pct`/`--max-procs` 时才有 cgroup 可改（见 `linux_limits.rs`），否则无处可写。做出来会时灵时不灵——与 F9.21 当初拒绝用 cgroup freezer 是同一条理由 |
| `--detach` | 有 | |
| 卷 / 绑定挂载 `-v` | 有 | F9.1，含 `:ro`；只读**递归到子挂载**（`mount_setattr(AT_RECURSIVE)`，门禁 V.2b/V.2c）|
| **命名卷** `volume create/ls/rm/inspect` | 有 | F9.35：`-v NAME:/path` 数据活得比容器久；卷就是 `~/.wbox/volumes/<名字>`，rootless 可用（门禁 VOL.1–VOL.6）|
| `--entrypoint` / `--env-file` | 有 | F9.36：覆盖镜像 Entrypoint（空串=清空）；env-file 让密钥不必进命令行（门禁 EP.1–EP.5）|
| 容器内工作目录 | 有 | F9.37：**修掉镜像 `WorkingDir` 与 `-w` 双双被静默丢掉的缺陷**；不存在时逐级创建，且建在容器可写层（门禁 WD.1–WD.5）|
| `ADD` / `ps --filter` | 有 | F9.38：`ADD` 自动解开本地 tar（远程 URL 明确不做）；`ps --filter status=|name=`（门禁 AF.1–AF.7）|
| **多阶段构建** | 有 | F9.39：`FROM ... AS <名字>` + `COPY --from=<名字>`；各阶段 config 独立，中间阶段不进最终镜像（门禁 MS.1–MS.5）|
| 端口映射 `-p` | 部分 | F9.2，**仅 TCP**；UDP/ICMP 做不到 |
| 镜像 pull/list/show/rm/inspect | 有 | |
| 镜像构建 | 部分 | F9.3 子集 + **分层缓存**（F9.5）；F9.36 补 `LABEL`/`EXPOSE`/`USER`/`ARG`，F9.38 补 `ADD`，F9.39 补**多阶段构建**；剩下不做的只有 `ADD` 的远程取回（理由见 F9.38） |
| overlay 可写层 | 有 | F9.12：运行期写入进 per-container upper，镜像缓存只读（门禁 OV.1–OV.8）；内核 <5.11 直接报错，不静默回退共享写入 |
| 镜像分层存储 | 有 | F9.16–F9.18：pull 保留原始层、可原样回推；build 产物 = 基础层+增量层（push 跳过基础层，PSH.8）；`FROM` 硬链接共享数据块（OVB.1–OVB.4）|
| 镜像 push | 有 | F9.16：pull 来的镜像**原样回推**，manifest digest 不变、分层保留（门禁 PSH.6–PSH.7）；build 产物无原始层，退回平铺单层（F9.13）|
| compose | 部分 | F9.14：八字段 + `up -d`/`down`/`ps`（门禁 CMP.1–CMP.7）；服务经共享 netns 互通，非 bridge+DNS |
| pod | 不做 | F9.15 之后 net/IPC/UTS 三种共享都能单独取得，再抽一层 pod 只是换个说法；理由见 §4.9 L6 |
| restart policy | 有 | F9.6：`no`/`on-failure[:N]`/`always`（门禁 R.1–R.4）|
| healthcheck | 有 | F9.10：`--health-cmd` + interval/retries/start-period（门禁 HC.1–HC.5）|
| 容器间通信 | 部分 | F9.11：`--network container:<NAME>` 共享 netns、localhost 互通（门禁 NC.1–NC.4）|
| namespace 共享（IPC/UTS）| 有 | F9.15：`--ipc`/`--uts container:<NAME>`，与 `--network container:` 同一套机制（门禁 IU.4–IU.7）|
| `--hostname` | 有 | F9.15：默认取容器名（docker 用容器 ID 前 12 位）|
| 自定义 bridge 网络、内建 DNS | 无 | rootless 下需要 slirp4netns/pasta 级的常驻用户态网络栈，与"免安装、无服务"（§2.2）冲突 |
| `--user UID[:GID]` | 部分 | F9.7：数字 id 生效（门禁 U.1–U.4）；**只映射一个 id**，用户名不支持 |
| `--cap-add` / `--cap-drop` | 有 | F9.8：先 drop 后 add，含 `ALL`（门禁 CAP.1–CAP.5）|
| seccomp | 部分 | F9.9：**拒绝名单**（门禁 SEC.1–SEC.6）；docker 用的是允许名单，二者边界强度不同 |
| Docker daemon 线协议兼容 | 不做 | §2.3 非目标；对标的是 CLI 与运行时行为 |

#### Q4 Linux 宿主 × Windows 程序 —— 对标 Wine

这一格的隔离能力**不是单独实现的**：wine 目标复用宿主程序模式的整条链路，
因此 Q3 的 F9 能力自动适用。取证放在宿主模式（H.6–H.9）而不是 wine 用例上——
后者缺 wine 就会 SKIP，而"缺环境时静默跳过"正是本项目吃过亏的地方（§4.9 W1）。

| 参照物特征能力 | wbox | 说明 |
|---|---|---|
| 运行 Windows CLI 程序 | 有 | 复用 Linux 隔离层调用系统 Wine |
| PE 判定与误判防护 | 有 | 看完整签名而非只看 `MZ`（门禁 W.4/W.5）|
| 在隔离内运行（Wine 本身不提供）| **有，这是 wbox 的增量** | Wine 只做 ABI 翻译，不做隔离 |
| 自带 Wine | 无 | 依赖宿主已装；缺失时明确报错 |
| `wineprefix` 与宿主隔离 | 有 | 用专用的 `~/.wbox/wineprefix`，不碰用户自己的 `~/.wine` |
| `wineprefix` **容器之间**隔离 | 有 | 每容器一个 prefix，置于其状态目录内，随容器记录一并清理（§4.9 L2）|
| GUI / DirectX / .NET | 不做 | §2.3 非目标（Wine 下 GUI 另议）|
| 隔离/限额/身份/capability/seccomp/健康检查 | 有 | **复用 Q3 同一条 Linux 链路**：wine 目标走宿主程序模式（`wrap_if_pe` 只替换最终 argv，隔离链路一字不差），故 `--user`/`--cap-*`/`--seccomp-deny`/`--restart` 一并生效。门禁 H.6–H.9 在宿主模式上取证（不依赖装 wine，故任何机器都真的会跑）|
| overlay 可写层 | 不适用 | F9.12 只对镜像模式（换根）有意义；wine 目标不换根 |
| `stats` | 有 | 走 `/proc` 那条路，不依赖换根也不依赖 cgroup；已实测宿主模式容器能取到 CPU/内存/进程数（F9.24）|
| `pause` / `unpause` | 有 | 同样只需进程树，与 Q3 同一实现（F9.21）|
| `diff` / `commit` / `cp` / `export` | 不适用 | 四者都要 overlay 可写层，而 wine 目标不换根——没有"相对镜像改了什么"这个问题；命令会明确报错说明是宿主程序模式，不是静默给空结果 |
| 暂停状态可见 | 有 | F9.32 判据是容器 init 是否为 `T`，与是否换根无关，故 wine 那一格同样看得见 |
| `inspect` 的 `Mounts` / `NetworkMode` | 有 | 宿主程序模式同样支持 `-v`/`-p`/`--network container:`，F9.33/F9.34 如实反映 |
| 命名卷 | 有 | 宿主程序模式支持 `-v`，故 `-v NAME:/path` 在 wine 那一格同样可用 |

#### 2.4.1 每格是怎么跑起来的

§2.4 的表说的是**有没有**，这一节说**靠什么跑的**。没有这一节，读者要从 §2.4
的状态表跳到 F2/F4/F5/F6 四个功能条目里自己拼，而四个象限恰恰是**两条隔离链路
乘以两种程序格式**的组合——说清这个组合，后面每一条能力落在哪一格就都是推得出来的。

**两条隔离链路**（宿主决定用哪条）：

| | Windows 宿主 | Linux 宿主 |
|---|---|---|
| 降权 | AppContainer SID + 低完整性级别 | user namespace（rootless） |
| 资源限额 | Job Object | cgroup v2（不可用时明确回退或拒绝） |
| 进程树回收 | Job `KILL_ON_JOB_CLOSE` | PID namespace + PDEATHSIG |
| 文件可见性 | **ACL 显式授权**（默认拒绝，`acl.rs`） | mount namespace + `pivot_root` |
| 网络 | 不授 `INTERNET_CLIENT` capability | network namespace |
| 对应功能条目 | F2 | F5 |

**两种程序格式**（目标决定怎么执行）：

| | 原生格式 | 异构格式 |
|---|---|---|
| Windows 宿主 | PE 直接由内核加载（Q1） | Linux ELF 经用户态 x86-64 执行器（Q2，F4；执行器**已是纯 Rust**（`crates/wbox-linux`），见 §2.2.1；剩余的是 guest ABI 覆盖缺口，不是语言迁移） |
| Linux 宿主 | ELF 直接由内核加载（Q3 的宿主程序模式） | PE 经系统 Wine（Q4，F6） |

**四格由此得出**：

| 象限 | 隔离链路 | 执行方式 | 关键实现点 | 门禁 |
|---|---|---|---|---|
| Q1 Win×Win | F2 | 内核直接加载 PE | rootfs 要**显式授 ACE** 才读得到；没有写重定向（§2.5 天花板一） | Windows 侧 WP.* |
| Q2 Win×Linux 镜像 | F2 | 用户态执行器跑 ELF | 双层隔离（AppContainer 套执行器）；免虚拟化是它存在的理由 | WP.*、F4.R6 产品门禁 |
| Q3 Linux×Linux 镜像 | F5 | 内核直接加载 ELF | 换根 + rootless overlay 可写层（F9.12）；F9 全套能力在这一格 | 本仓 `scripts/test-linux-backend.sh` 绝大多数组 |
| Q4 Linux×Win 程序 | F5 | `wrap_if_pe` 换掉最终 argv，交给系统 Wine | **隔离链路一字不差地复用 Q3 的宿主程序模式**，故 F9 的身份/限额/capability/seccomp 自动适用；每容器独立 `wineprefix` | W.1–W.5（PE 判定与执行）、H.6–H.12（能力取证） |

**从这张表能直接读出三件事**，省得每加一个能力都要重新论证一遍：

1. **一个能力落在哪几格，取决于它依赖哪条链路的哪个原语。** 依赖 overlay 可写层的
   （`diff`/`commit`/`cp`/`export`）只在 Q3 成立，因为只有那一格换根；依赖 `/proc`
   或进程树的（`stats`/`pause`）在 Q3 与 Q4 都成立，因为两格共用 F5 那条链路。
2. **Q4 的能力不必单独实现，但必须单独取证。** 复用不等于验证过——H.6–H.12 正是
   在宿主程序模式上取证的，而不是在 wine 用例上（缺 wine 就会 SKIP，而"缺环境时
   静默跳过"是本项目吃过亏的地方，见 §4.9 W1）。
3. **Q1/Q2 的缺口大多不是工作量问题，而是链路里没有那个原语。** Windows 侧没有
   overlay 可写层，`diff`/`commit`/`cp`/`export` 就无从谈起；没有 uid 映射，
   `--user` 就只能明确报错。这类缺口写成"不做"并给出原语层面的理由，
   比列成待办诚实（§2.4.3 的下一步表逐条给了理由）。

#### 2.4.2 与四个参照物的架构对照

§2.4 说 wbox **有什么**，§2.4.1 说 wbox **怎么做的**。这一节说**对标物是怎么做的、
差在哪、为什么**——不写这一节，「对标 Sandboxie-Plus」这句话就只是个口号，
读者无从判断哪些差距是暂时的、哪些是架构决定的。

每格三问：对标物的**介入点**在哪？wbox 的介入点在哪？由此**必然**产生哪些差异？

---

##### Q1 对照 Sandboxie-Plus

| | Sandboxie-Plus | wbox |
|---|---|---|
| 介入点 | **内核态**：文件系统 minifilter 驱动 + 注册表回调，逐个拦截 I/O 请求并改写目标 | **用户态**：进程创建时贴上 AppContainer SID + 低完整性级别，之后由内核按 ACL 判定 |
| 装什么 | 装驱动，要管理员权限 | 不装任何东西，普通用户即可 |
| 写重定向 | **有**：写入被改写到沙箱目录，原文件不动（真正的 copy-on-write） | **无**：只能「拒绝」，不能「改写到别处」 |
| 注册表 | 同上，虚拟化 | 无 |
| 强制入盒 | 有（Forced Programs：指定路径的程序自动进盒） | 无（要全局钩子或驱动） |
| GUI 程序 | 支持 | §2.3 非目标 |
| 资源限额 | 不强调 | 有（Job Object：内存/CPU/进程数） |
| 进程树回收 | 有 | 有（Job `KILL_ON_JOB_CLOSE`） |

**差异是介入点决定的，不是工作量决定的。** 原生 PE 程序发的是真 NT 调用，
用户态没有任何位置能把 `CreateFile` 的目标路径换掉——除非注入进目标进程（脆弱、
易被绕过，且本身就是一种被杀软盯上的行为）或装驱动（§2.5 天花板一，与「免安装、
不要管理员权限」这个产品前提直接冲突）。

**所以 Q1 兑现的是另一半：**「默认拒绝 + 显式授权」。Sandboxie 的模型是
「随便写，写到别处去」；wbox 的模型是「不给授权就读不到、写不了」。对
「跑一个不信任的 CLI 工具、别让它翻我的文档」这个主场景，后者够用且更简单；
对「让程序以为自己改了系统、其实没改」这个 Sandboxie 的招牌场景，wbox 给不了。
§4.9 W3 已完成真机取证：VirtualStore 不生效；用户态近似止于默认拒绝、显式
授权和 package 私有标准目录。

---

##### Q2 对照 WSL2 / Docker Desktop

| | WSL2 | wbox |
|---|---|---|
| 介入点 | **虚拟机**：Hyper-V 轻量 VM 里跑一个**真 Linux 内核** | **用户态执行器**：在 Windows 进程内解释/JIT 执行 x86-64 指令，syscall 由执行器实现 |
| 装什么 | 启用 Hyper-V/虚拟化，要管理员权限，部分机器 BIOS 禁用虚拟化则完全不可用 | 不装任何东西 |
| syscall 覆盖 | 完整（就是真内核） | 子集，缺口逐条列在 F4（异步信号语义、glibc pthread/clone、ptrace） |
| 性能 | 接近原生 | **不可比**（§2.5 天花板二）：用户态解释/JIT |
| 文件互通 | 9p/virtiofs 跨 VM 边界 | 同进程内的 VFS，无跨边界开销 |
| 网络 | VM 内独立网络栈 + NAT | **没有独立网络栈**：guest 的 socket 就是宿主 socket，隔离靠 AppContainer 不授 `INTERNET_CLIENT`（能力开关，见 §4.9 W5） |
| 隔离 | VM 边界 | AppContainer 套执行器（双层） |

**wbox 在这一格存在的唯一理由是「不要虚拟化」。** 目标用户是「受管 Windows 机器
上的开发者」（§3.1）——那种机器上 Hyper-V 常被策略禁用，或者根本没有管理员权限。
在能开 WSL2 的机器上，WSL2 更快更完整，wbox 不主张替代它。

**由此也定了取舍的方向**：凡是要靠虚拟化才能拿到的（完整 syscall、原生性能、
独立网络栈），一律不做；凡是用户态能做的（rootfs、镜像、隔离、限额），做到位。

---

##### Q3 对照 Podman / Docker

| | Podman（rootless） | Docker | wbox |
|---|---|---|---|
| 介入点 | 内核 namespace + cgroup，无守护进程 | 同左，但经守护进程 | **同 Podman**：内核 namespace + cgroup，无守护进程 |
| 装什么 | 装 podman | 装 docker daemon（通常要 root） | 单个可执行文件 |
| 网络 | netavark/slirp4netns，有 bridge 与内建 DNS | 同左 | **只有 namespace 级隔离**；bridge 与 DNS **不做**——它们在 rootless 下要常驻用户态网络栈，与「免安装、无服务」冲突 |
| pause | cgroup freezer | 同左 | SIGSTOP/SIGCONT（cgroup 只在设了限额时才存在，见 F9.21）——差异已逐条写明 |
| pod | 有 | 无 | **不做**（§4.9 L6 已评估：F9.15 补齐 IPC/UTS 后，三样共享都能单独取得） |
| 镜像/构建/生命周期 | 全套 | 全套 | F9.1–F9.36 覆盖绝大部分，逐条见 §2.4 的 Q3 表 |

**这一格差距最小，因为介入点完全相同**——都是内核 namespace。剩下的差异只有两类：
要常驻服务的（bridge/DNS）与要更强内核特性的（freezer 需要 cgroup 存在）。
两类都在 §2.4.3 里写明了不做的理由，不是待办。

---

##### Q4 对照 Wine

| | Wine | wbox |
|---|---|---|
| 介入点 | **ABI 翻译**：在 POSIX 之上实现 Win32 API，自带 PE loader | **不重复造这一层**：直接调用系统 Wine 跑 PE |
| 隔离 | **没有**。Wine 进程就是普通 Linux 进程，能读写你的整个 `$HOME` | **wbox 的增量就是这个**：PE 进程跑在 Q3 那条完整的 Linux 隔离链路里 |
| `wineprefix` | 默认 `~/.wine`，多个程序共用 | **每容器一个**，放在容器状态目录内，随容器记录一并清理（§4.9 L2） |
| 限额/身份/capability/seccomp | 无 | 有——复用 Q3 同一条链路（门禁 H.6–H.12 取证） |
| 自带 Wine | —— | **不做**：分发体积与许可都不划算，缺失时明确报错 |
| GUI / DirectX / .NET | Wine 的主战场 | §2.3 非目标 |

**「对标 Wine」这句话要拆开看**：ABI 翻译那一半 wbox 完全不做（做了是重复造轮子，
而且做不过 Wine）；**隔离那一半 Wine 完全不做**，正是 wbox 补上的。所以这一格
不是「追赶 Wine」，而是「给 Wine 加一层它本来就没有的东西」——PRD §2.4 的 Q4 表里
「在隔离内运行（Wine 本身不提供）」那一行标的是「**有，这是 wbox 的增量**」。

---

**四格合起来看，wbox 的定位是一条线**：不装驱动、不要虚拟化、不起守护进程，
在这个前提下把四个象限各自能兑现的部分做到位，兑现不了的**在 PRD 里写明白为什么**。
这条线也解释了为什么 §2.4.3 的「不做」清单里，判断原则始终是同一条——
要装驱动 / 要常驻服务 / 要虚拟化的，一律不做。

#### 2.4.3 每格的下一步

上面的表说的是**现在在哪**，这一节说**接下来往哪走**。没有这一节，"对标"就只是
一张状态表，看不出哪些缺口是打算补的、哪些是永远不补的。

| 象限 | 还差什么 | 打算怎么办 | 归属 |
|---|---|---|---|
| Q1 Sandboxie | 文件/注册表写重定向 | **已取证，不做任意路径重定向**；保留 package 私有标准目录近似 | §4.9 W3，已结 |
| Q1 Sandboxie | 命名沙箱内容、Forced Programs | **不做**；前者缺持久 copy-on-write 层，后者需要驱动或全局钩子 | §4.9 W3，已结 |
| Q2 WSL2 | 卷挂载 `-v` | broker 逐项打开对象 HANDLE + 模拟器 VFS 数据面，**绕开**驱动级路径重定向 | §4.9 F9.1，Windows agent |
| Q2 WSL2 | 端口映射 `-p` | **已取证，结论是语义不适用**：guest 绑的就是宿主端口 | §4.9 W5，已结 |
| Q2 WSL2 | syscall 覆盖缺口 | 按 F4 逐条补（异步信号语义、glibc pthread/clone、ptrace） | Windows agent |
| Q3 Podman | —— | F9.1–F9.39 已全部完成并各有门禁 | — |
| Q3 Podman | pod | **已评估，不做**：F9.15 补齐 IPC/UTS 后，pod 的三样共享都能单独取得 | §4.9 L6，已结 |
| Q3 Podman | ~~多阶段构建~~ | **已完成**（F9.39，门禁 MS.1–MS.5）：按上述做法实现——每个 `FROM` 切换产物目录，非最终阶段用临时目录，多阶段时禁用前缀缓存并出声 | — |
| Q3 Podman | 自定义 bridge、内建 DNS | **不做**：rootless 下需常驻用户态网络栈，与 §2.2「免安装、无服务」冲突 | — |
| Q3 Podman | `events` | **不做**：需要常驻事件流与订阅端，wbox 没有 daemon 可发事件——与上一条撞的是同一堵墙 | — |
| Q3 Podman | `update` 改运行中容器的限额 | **不做**：没设限额就没有 cgroup 可改，做出来会时灵时不灵；与 F9.21 拒绝用 freezer 同一条理由 | — |
| Q4 Wine | 自带 Wine | **不做**：分发体积与许可都不划算，缺失时明确报错即可 | — |
| Q4 Wine | GUI / DirectX / .NET | **不做**：§2.3 非目标 | — |

一条判断原则：**凡是要装驱动、要常驻服务、要虚拟化的，都不补**（§2.2/§2.5）。
补的只有那些在"免安装、无服务、无驱动"前提下真能做成的。

#### 2.4.4 每格的能力路线图

§2.4.3 说「下一步做什么」，粒度是缺口；这一节把四格拉到**同一深度**：每格的能力
序列在哪、还剩什么、判据是什么。补这一节的直接原因是四格深度本来不齐——Q3 有
F9.1–F9.39 逐条带门禁，Q2 有 F4.R0–R7 的纵切图，**Q1 却只有一张对照表**，
读者无从知道那一格接下来能做什么。

---

##### Q1（Windows × Windows 程序）路线图

现状序列是 F2.1–F2.7（AppContainer profile、Low IL、capability、Job 限额、
挂起创建、进程树回收、profile 清理），**已完成且有 Windows CI 门禁**。

再往前的能力分三类，**必须先分清**，否则会把「架构上不可能」的东西一直挂在待办里：

| # | 能力 | 类别 | 说明与判据 |
|---|---|---|---|
| Q1.1 | 写重定向的**用户态可行性取证** | **已完成**（W3） | 32 位无 manifest Rust 探针写 Program Files 得到 Win32 5，真实路径和 VirtualStore 均无文件；普通非 UWP 进程的 `LOCALAPPDATA` 被改到 package `AC` 且可写 |
| Q1.2 | 基于取证结论的写重定向近似 | **边界已定** | 任意路径 copy-on-write 不做；保留默认拒绝和显式 ACL 授权，标准 `LOCALAPPDATA`/`TEMP`/`TMP` 使用 AppContainer package 私有存储（WP.25/WP.27） |
| Q1.3 | 临时目录私有化（`TMPDIR`/`TEMP`/`TMP` 指向容器私有目录）| **已实现**（`--private-tmp`，门禁 PT.1–PT.5、WP.25）| Linux 三项均指向状态目录；Windows 的 `TMPDIR` 指向状态目录，AppContainer 将 `TEMP`/`TMP` 改到 package 专属 `AC\Temp`。两处均已实机验证可写，且显式 `-e TMPDIR=...` 优先。边界：只覆盖遵守该约定的程序 |
| Q1.4 | 授权粒度：只读授予 | **已验证**（W7）| AppContainer 真机探针证明 RX ACE 下读取成功，覆盖与新建均返回 `PermissionDenied`；共享镜像缓存已使用该粒度。它不提供路径映射，不能据此宣称 Windows 原生 `-v :ro` 已实现 |
| Q1.5 | capability 粒度 | **取证中**（W8）| 三种 well-known SID 均可构造；实测 `bind` 在无 capability 时也成功，同机私网流量又受 loopback isolation 阻断，二者都不能证明 server/private 能力。外部 private peer 门禁已编码，流量证据前不开放 CLI |
| — | 注册表虚拟化 | **不做** | 与写重定向同一介入点问题（§2.4.2 Q1） |
| — | 强制入盒（Forced Programs）| **不做** | 需全局钩子或驱动（§2.5 天花板一） |
| — | GUI 程序沙箱 | **不做** | §2.3 非目标 |

Q1.3 已经落地了——它提醒了一件事：**「待 Windows 验证」的条目要先拆开看，
机制是不是真的依赖那台机器**。Q1.3 的机制（建目录 + 注入环境变量）完全平台中立，
硬挂在 Windows 侧只是因为它写在 Q1 的表里。拆开之后，能在这边做的就该在这边做完，
只把真正需要那台机器的部分留过去。

**Q1.4–Q1.5 标着「待 Windows 侧确认」不是客套**：本文由 Linux 侧 agent 写，
这三条的可行性判断来自对 Windows 机制的了解而非实测。**接手的 Windows agent
应当先验证再实现**，验证不成立就把条目改成「不做」并写明原因——这比留一个
永远做不成的待办诚实。

---

##### Q2（Windows × Linux 镜像）路线图

**已有完整序列，见 F4**：`F4.R0` 删 Blink/C 依赖 → `R1` 纯 Rust ELF64 loader 与
虚拟内存 → `R2` 纯 Rust x86-64 执行器 → `R3` syscall/fd/信号/进程模型 →
`R4` guest VFS/rootfs/`/proc`/`/dev`/只读卷 → `R5` 动态 glibc/线程/fork·exec/
epoll/socket → `R6` Alpine·Ubuntu 24.04 产品门禁 → `R7` 删除 `vendor/blink`。

从**用户能拿到什么**的角度重述这条序列（F4 那份是实现视角，两份说的是同一件事）：

| 里程碑 | 用户拿到的 |
|---|---|
| R1–R2 `[done]` | 能加载并执行静态 ELF |
| R3 `[部分]` | 有 syscall/fd/进程模型，静态程序可跑通；**信号投递与线程未做** |
| R4 `[部分]` | 有 rootfs 与 `/dev`；`-v` 只读卷仍是「计划重做」 |
| R5 `[部分]` | 动态 glibc、`fork`/`exec`/管道可跑；线程、`MAP_SHARED`、socket/epoll 未做 |
| R6 `[部分]` | Ubuntu 24.04 已进入 Windows 产品门禁（WU.1/WU.2）；Alpine 仍只有手工跑通证据 |
| R7 `[done]` | 仓库里不再有 C 依赖，§2.2.1 第一档达成 |
| R9 `[done]` | §2.2.1 **第二档**：承载产品语义的第三方 crate 全部换成第一方，含 TLS。构建图只剩五个 wbox 包 + 平台 ABI |

**F9.37 的 `guest_workdir` 已经放进 `RunSpec`**，Q2 的镜像路径在 R4 落地时读它
即可，不必再改结构——这是 Q3 的实现顺带给 Q2 铺的路，也是四格共用一套
`RunSpec` 的价值。

---

##### Q3（Linux × Linux 镜像）路线图

**F9.1–F9.39 已全部完成并各有门禁**（逐条见 §2.4 的 Q3 表与 §4 的 F9 树）。
剩下的都是**已论证的不做**：自定义 bridge 与内建 DNS（要常驻用户态网络栈）、
pod（§4.9 L6：F9.15 补齐 IPC/UTS 后三样共享都能单独取得）、`events`（要常驻事件
流）、`update` 改运行中容器的限额（没设限额就没有 cgroup 可改）、`ADD` 的远程取回
（构建期出网拿不到缓存与校验）。

这一格的「下一步」不再是补动词，而是**守住已有行为**：每加一个能力都要问它落在
哪几格（§2.4.1 的判断法），并给已写进文档的差异配门禁（如 PZ.6、AF.3），
让实现和文档不能各走各的。

---

##### Q4（Linux × Windows 程序）路线图

**能力上已经到位**：隔离链路复用 Q3 的宿主程序模式，F9 的身份/限额/capability/
seccomp/`stats`/`pause` 自动适用，取证在 H.6–H.12（落在宿主程序模式上，不依赖装
wine，任何机器都真的会跑）；`wineprefix` 每容器独立（§4.9 L2）。

剩下的两项都是**不做**：自带 Wine（分发体积与许可都不划算，缺失时明确报错）、
GUI/DirectX/.NET（§2.3 非目标）。

所以这一格的路线图是**空的，而且是好事**：§2.4.2 已经说清，wbox 在这格补的是
「Wine 本来就没有的隔离」，那一半已经补完；ABI 翻译那一半交给 Wine，本就不该做。

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
| F2.4 Windows 资源限制 | `job.rs` | G4 | WP.26 通过公开 `run --memory` 对照证明同一 workload 只在 Job 总内存限额下触发 OOM |
| F2.6 Windows 进程树回收 | `job.rs`、`sandbox.rs` | G4 | WP.9、WP.15、WP.17 分别覆盖 stop、并发 exec 与 supervisor 崩溃后的整树回收 |
| F3.1-F3.4 引用、认证、manifest、digest | `src/oci` | G2 | Rust 严格错误测试 + 可选 pull；真实 pull 不能替代离线失败路径 |
| F3.5-F3.7 层、链接和路径 | `oci/image.rs` | G2 | 构造 tar 与真实 Alpine 3.20 applet 链接通过；dangling symlink 仍有缺口 |
| F3.8/F3.9 缓存管理与 config 合并 | `src/oci`、`cli/image.rs` | G2 | 缓存仅以 `rootfs` 目录判完成，失败/并发 pull 原子性未门禁 |
| F4.R0 移除 C/C++ runtime | 全仓、CI、发布脚本 | `[done]` | `vendor/blink` 已删除；TLS 去 OpenSSL；CI 不再装 C 工具链 |
| F4.R1-F4.R4 ELF/CPU/syscall/VFS | `crates/wbox-linux` | G1 | `cargo test -p wbox-linux`（173 项：90 单测 + 61 指令语义 + 22 端到端）；实测跑通静态/动态 glibc、busybox、Alpine 镜像、shell 的 fork/exec 与管道。VFS 的宿主符号链接逃逸已封死（用户态 `RESOLVE_IN_ROOT`）；x87/socket/MAP_SHARED 仍是缺口，见 `docs/rust-rewrite.md` §4 |
| F4.R8 合并成单一 `wbox.exe` | `src/runtime` + `EmuBackend` | `[planned]` | 见 §4.9 R8：需先决定进程内执行如何保留"AppContainer 套模拟器"的双层隔离 |
| F4 Windows 完整 Linux guest 路径 | Rust runtime + F2/F3 | G3 | `WP.3`：portable artifact 在 AppContainer 内执行 BusyBox（当前仍是两个 exe） |
| F5.1-F5.5 namespace/fs/network | Linux backend | G3 | L1/H/N，CI 使用 REQUIRE |
| F5.6/F5.7 cgroup/rlimit | `linux_limits.rs` | G3 正常路径 | C/L2；溢出、spawn 失败清理和跨后端内存语义仍有缺口 |
| F5.8 后代清理 | `linux_ns.rs` | G3 | L3.1/L3.2 |
| F6.1-F6.3/F6.5 PE/Wine 分派 | `wine.rs` | G3 | W.1/W.2/W.4/W.5 |
| F6.4 隔离、网络和限额复用 | F5 + `wine.rs` | G3 部分 | W.3 覆盖网络；缺 PE workload 的资源超限行为断言 |
| F7.1-F7.5 环境与凭证 | `backend/env.rs`、`registry.rs` | G2/G3 部分 | Rust 严格测试 + `WP.2`；Linux image 与 Windows image 路径仍随各自 G3 |
| F8.1 状态目录与 `ps` | `runstate.rs`、`cli/ps.rs` | G3 | P.1-P.5、`WN.8`、`WNET.4` 与 `WP.5` |
| F8.2/F8.3 detach/logs/stop/rm | `src/cli/run.rs`、`logs.rs`、`stop.rs`、`runstate.rs` | G4 Windows / G3 Linux | P.6-P.18、WP.6-WP.12；`WP.7A` 新增 detached `--rm` |
| F8.4 exec | `src/cli/exec.rs` | G4 Windows / G3 Linux | Linux P.19-P.22；Windows 原生目标 WP.13-WP.17；CI 30250676453 通过 |
| F8.7 create/start | `src/cli/create.rs`、`start.rs`、`runstate.rs` | G3 Linux / G4 Windows | P.25/WP.21/WP.24：create 不执行，start 原子领取配置，rename 后按新名称启动，退出后可再次启动 |
| F8.8 detached 管道 EOF | `src/cli/run.rs` | G4 Windows | WP.22：重定向输出及时 EOF且 workload 继续运行；提交 `55761da`、CI 30272887266 |
| F9.1 bind volume | `linux_ns.rs`、guest VFS | G3 Linux | Linux V.1–V.4 + V.2b/V.2c（`:ro` 递归到子挂载）。**模拟器侧无证据**：`t_mount_ro` 在当前纯 Rust 引擎上是**已知失败**（`mount(2)` 未实现，`tests/known-failures.txt` E 组），此前"模拟器 `t_mount_ro` 覆盖 `MS_RDONLY`"是 Blink 时代的说法。Windows OCI 的 `-v` 仍未开放（`[TODO-WINDOW]`）|

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
- `WP.22`：长命 workload 运行期间，`run -d`、`container start` 与顶层 `start`
  的重定向 stdout/stderr 必须及时 EOF，调用方不得等到容器退出。
- `WP.23A/WP.23B`：detached 父进程必须等到真实 workload READY；缺失 Windows
  程序与离线 pull 分别保留 spawn/registry 原始错误类别和非零退出码，且不留状态记录。
- `WP.24`：`create old -> rename old new -> start new` 必须以新名称建立
  supervisor、Job 与状态记录；保存配置中的旧 `--name` 不得复活。
- `WP.25`：`--private-tmp` 下状态目录 `TMPDIR` 与 AppContainer package
  `TEMP`/`TMP` 均可实际写入和读回；显式 `-e TMPDIR=...` 必须优先。
- `WP.26`：同一 PowerShell 分配 workload 在无限额对照中完成 192 MiB 分配，
  在 `--memory 64` 下必须捕获 `OutOfMemoryException`，证明 Job 限额改变真实行为。
- `WP.27`：普通非 UWP Windows 程序看到的 `LOCALAPPDATA` 必须位于本容器
  package 的 `AC` 下且可写，宿主真实 `LOCALAPPDATA` 不得出现该文件。

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
    ├── F1.7.11 `create [RUN OPTIONS]` 保存配置但不运行，`start NAME...` 启动或重启
    └── F1.7.12 detached 父命令可安全用于管道/命令替换，不被 supervisor 拖住 EOF
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

> 状态标记从 `[active: Rust runtime replacement]` 改为 `[active]`：**替换本身
> 已经完成**（引擎是 `crates/wbox-linux`，`vendor/blink` 已删除，F4.R0/R1/R2/R7
> 均 `[done]`）。这条仍是 `[active]`，缺的是 **guest ABI 覆盖**（R3–R6），
> 不是语言迁移。

```text
F4
├── F4.R0 `[done]` 删除 Blink/C 产品依赖与构建链（含 native-tls -> 纯 Rust TLS）
├── F4.R1 `[done]` 纯 Rust ELF64 loader、虚拟内存和初始进程栈（含 auxv）
├── F4.R2 `[done]` 纯 Rust x86-64 解释执行器（整数全集 + SSE/SSE2）；JIT 未做
├── F4.R3 `[active]` Linux syscall 与 fd 已可用（`syscall/mod.rs` 的分发表
│                   现有 76 条）；进程族（快照式 fork/execve/wait4）已做，
│                   信号投递未做
├── F4.R4 `[active]` guest VFS 与 rootfs 前缀约束已做，且**已从词法检查升级为
│                   用户态 `RESOLVE_IN_ROOT`**（逐段解析、符号链接目标重新从
│                   根展开，见下方"宿主符号链接逃逸"）；/dev/{null,zero,full,
│                   random,tty} 与 /proc/self/exe 已合成，procfs 其余未做
├── F4.R5 `[active]` 动态 glibc、shell 的 fork/exec 与管道已跑通；
│                   线程、epoll、socket、MAP_SHARED 跨进程共享未做
├── F4.R6 `[active]` Ubuntu 24.04 已接 WU.1/WU.2；Alpine 仍缺正式产品门禁
└── F4.R7 `[done]` vendor/blink 已从仓库、CI、文档和发布物删除
```

首个纯 Rust 纵切落在 `src/runtime`（`MOV r64,imm` + `syscall`，跑手工构造的
`exit(42)`）。此后引擎移到独立 crate `crates/wbox-linux` 并补齐到可用：
整数指令全集、SSE/SSE2（含浮点）、`syscall/mod.rs` 分发表现有的 76 条
syscall、带逃逸防护的 VFS。
实测跑通静态与动态 glibc 程序、仓库内静态 busybox 多个 applet、真实动态
coreutils，以及 `wbox image pull alpine:3.20` 后其中的动态 musl PIE busybox。
门禁 `cargo test -p wbox-linux` 现为 173 项（88 单测 + 63 指令语义 + 22 端到端）；
`src/runtime` 现在是同一引擎的**进程内入口**，不再是第二份实现。
指令解码是自己写的（`crates/wbox-linux/src/exec.rs`），不依赖外部 decoder crate，
因此 `iced-x86` 依赖已移除。

**宿主符号链接逃逸（曾是 Critical，已修）**。VFS 早先只做**词法**规范化：
挡得住 `../../etc/passwd`，却完全挡不住符号链接——rootfs 里一个 `/evil -> /`
的链接，guest 打开 `/evil/etc/shadow` 时词法上一路合法，内核在宿主上跟着链接走，
直接读到宿主的 `/etc/shadow`（修复前 `t_sec_path` 报的就是
"SANDBOX ESCAPE: open(...) succeeded"）。现改为逐段解析、符号链接目标重新从
rootfs 根展开，`..` 与绝对目标都作用在"已解析栈"上，栈空即到根——**结构上不可能**
指到 prefix 之外，语义等同内核的 `openat2(RESOLVE_IN_ROOT)`。不直接用
`openat2` 是因为它只在 Linux 5.6+，而本 crate 也要在 Windows 宿主上跑；
一套可移植实现两边共用。代价与残余风险（每段一次 `symlink_metadata`、
check-then-use 的 TOCTOU 窗口）见 `docs/rust-rewrite.md` §4 第 9 条。
基线随之收紧：`t_sec_path` 与 `t_sec_linkabs` 移出 `tests/known-failures.txt`，
四条安全用例全部在基线之外。

F4.R0 已收口：`vendor/blink=452` 个 native 源全部删除；`native-tls ->
openssl-sys` 这条 Linux 侧 C 依赖先换成 rustls，**现已换成自实现的
`crates/wbox-tls`**（§2.2.1 第二档，安全声明见 §2.2.2）。仓库里只剩
`tests/guest` 下的 21 个
`.c`，它们是**被模拟执行的 guest 夹具**、不进任何发布物；按下面的计划仍应改写成
no_std Rust，但那不影响 §2.2.1 的发布验收。

> **以下"vendor/blink Rust 替换图"是历史记录**（迁移期的规划），保留是因为它
> 说明了"哪些东西决定不迁移、为什么"。图中提到的 `flate2` 等第三方 crate
> **后来也被换成第一方实现**（§2.2.1 第二档），当前依赖以 §2.2.1 的替换清单为准。

**vendor/blink Rust 替换图**（历史）：

```text
不迁移，Rust 接管后直接删除
├── blinkenlights TUI、反向调试、BIOS/i8086/显卡模拟
├── 非 x86-64 宿主的 JIT 模板与交叉工具链
├── third_party 测试镜像、摘要和下载脚本
└── libz C；产品 OCI gzip 当时改用 flate2，现已换成第一方 wbox-codec::deflate

按产品纵切迁移
├── R1 自研 decoder + Rust CPU/register/flags/instruction semantics
├── R2 ELF64、动态加载、guest memory、stack/auxv、mmap/brk
├── R3 Linux syscall dispatcher、errno、fd 与时间
├── R4 Rust VFS/rootfs/proc/dev、路径边界、volume ro/rw
├── R5 fork/clone/exec、线程、futex、信号与 wait
├── R6 socket、poll/epoll、eventfd/timerfd/signalfd
└── R7 可选纯 Rust JIT；解释器产品门禁通过前不引入
```

`tests/guest/*.c` 先改为 no_std Rust x86-64 Linux fixtures，作为每个纵切的验收
——这一条**仍未做**，21 个夹具还是 C（它们是被模拟执行的 guest 输入，不进发布物，
故不违反 §2.2.1）。"迁移期 Blink 作为差分 oracle"这一条已随 `vendor/blink`
删除而失效，不再适用。

以下 Blink 结果仅作为纯 Rust 迁移的行为基线，不再证明目标架构完成。历史上直接
运行 `wbox-linux.exe` 的 G1 组件测试已覆盖主流单线程 CLI、动态 glibc
程序、shell 管道/命令替换/后台任务、fork 子 DNS 和 `apt-get update`。这些
结果不再表述为 Windows 产品路径已完成；`EmuBackend` 经 AppContainer 的 G3
仍由 WP.3 裁决。组件层仍有限制：

- 宿主异步信号语义不完整。
- glibc pthread/通用 clone 尚未支持。
- ptrace 未支持。

> **以下"F4.3 的覆盖缺口"一段是 Blink 时代的记录**（`PortableMmap`、
> `W32Mmap64`、`blink.dat.XXXXXX` 都是那份已删除的 C 实现里的东西）。
> 保留它不是为了描述当前实现，而是为了留住那条**至今仍然成立的教训**：
> 产品首页宣传的路径可以长期零覆盖，直到有人给它加一条门禁才暴露。
> 当前引擎的匿名映射由 `crates/wbox-linux` 自己的 `mem.rs` 处理，与下文无关。

**F4.3 的覆盖缺口（2026-07-27 发现并修复；Blink 时代）**。`wbox run <镜像>` 走的是
guest 前缀环境变量（首选名现在是 `WBOX_PREFIX`，`BLINK_PREFIX` 保留为兼容名，
两者引擎都认，见 `crates/wbox-linux/src/syscall/fs.rs`；下文沿用当时的
`BLINK_PREFIX` 写法），而 `scripts/test-matrix.sh` 的 A–F 组**全部不设**
该前缀（guest 的 `/` 直通宿主 `/`）。也就是说产品首页宣传的
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

> **以下三段取证（Ubuntu 24.04 / Fedora 42 / Python 3.12）产生于 Blink 时代**，
> 记录的是那条已删除的 C 路径能跑到哪一档，作为**迁移基线**保留，不代表当前
> 纯 Rust 引擎的能力。当前引擎的实测状态见 §2.4 Q2 表与 `tests/KNOWN-FAILURES.md`：
> Python 在当前引擎上**跑不通**（默认命令未按 PATH 搜索；给绝对路径后撞上未实现的
> `0f ae /3`）。两者冲突时以 §2.4 Q2 表为准。

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
模拟器/Linux ABI、线程或同步原语兼容缺口，必须以有界超时门禁继续定位，当前
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
`docs/rust-rewrite.md`，问题台账见 `tests/KNOWN-FAILURES.md`。

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

`create/start`、`--detach`、`wbox ps/stop/kill/top/rm/logs/exec/wait/inspect`。这是 wbox 离"能当 harness 的长期
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

**F8.f detached 父命令的标准句柄边界**。Windows 上 `std::process::Command`
即使把 supervisor 的 stdout/stderr 显式指向日志，仍可能顺带继承父 wbox 当前
标准句柄。PowerShell native-command pipeline 的管道句柄若被长命 supervisor
持有，`wbox run -d ... | Out-String` 或命令替换就会一直等不到 EOF。

启动 supervisor 前必须在进程级互斥区内暂时清除 stdin/stdout/stderr 的
`HANDLE_FLAG_INHERIT`，spawn 后由 RAII 原样恢复；`Command` 为日志文件准备的
显式 stdio 句柄不受影响。修改标准句柄属性是进程全局操作，因此不允许两个
内部 spawn 窗口并发交错。WP.22 同时等待短命父进程退出和两个重定向流 EOF，
并在随后确认 workload 仍存活，防止以“容器也提前退出”伪造通过。

**F8 的覆盖现状（如实记录）**。Linux 由 P.1–P.25 覆盖完整生命周期；
Windows 由 WP.6–WP.24 覆盖 detach、ps、logs、stop、rm、kill/top、create/start、
rename 后启动、READY/ERROR 握手、管道 EOF 与原生 exec，其中
WP.17 直接证明 supervisor 崩溃时主 guest 和 exec guest 均被 Job 回收。Windows
OCI/模拟器 exec 不在承诺范围，必须明确拒绝。

detached supervisor 在释放 owner 锁前把 guest 退出码写到状态目录的
`exit-code`；`wbox wait NAME...` 等待锁释放后打印该值，`inspect` 的
`State.ExitCode` 使用同一来源。异常崩溃和旧版残留没有可信退出码时必须报告
unknown，不能编造为 0。`wbox inspect`、`image inspect` 与
`container inspect` 输出 JSON 数组；镜像内疑似凭证的 Env 仍脱敏。

**F8.d 两侧可对齐范围**。`ps/stop/rm/logs/--detach` 语义可完全对齐。
`exec` 只能部分对齐：Linux 进入已有 namespace；Windows 原生目标重新使用同一
AppContainer SID、网络 capability 与命名 Job，并继承记录的工作目录。Windows
OCI/模拟器 的 rootfs 与镜像环境无法可靠重建，明确拒绝。原生 exec 也不继承
原 run 的自定义环境：状态文件刻意不落环境变量或凭证；需要这类语义时应由未来
的 supervisor 控制通道传递，而不是把秘密写入 `meta.json`。

分期与验收（每期都要有持续执行的门禁断言，理由见 §6 的覆盖教训）：

| 期 | 范围 | 验收 |
|---|---|---|
| F8.1 `[done]` | 状态目录 + `wbox ps`（只读） | P.1–P.5、WN.8、WNET.4 与 WP.5 已通过；跨进程 register/rm 竞态 G0 与 CI 30250676453 通过 |
| F8.2 `[done]` | `--detach` + `logs` | **已完成**（门禁 P.9–P.14）：detach 立即返回、容器后台续跑、stdout/stderr 分别落盘可读、退出后保留记录供事后查看、体积有界且截断可见 |
| F8.3 `[done]` | `stop` / `rm` | **已完成**：`stop` 收走整棵进程树（P.15，3→0 后代）、状态转 exited 并保留（P.16）、幂等（P.17）、不存在时报错（P.18）；`rm` 拒绝删存活容器（P.6/P.7/P.8）|
| F8.4 `[done]` | `exec` | Linux P.19-P.22 与 Windows 原生 WP.13-WP.17 在 CI 30250676453 通过；Windows OCI/模拟器 明确拒绝 |
| F8.5 `[done]` | `wait` + container/image `inspect` | Rust 跨平台状态测试；Windows 双 exe 产品路径 WP.7B/WP.7C |
| F8.6 `[done]` | `kill` + `top` | Linux P.23/P.24；Windows WP.19/WP.20；Windows `top` 查询 Job 成员，`kill` 清空三层进程树 |
| F8.7 `[done]` | `create` + `start` | Rust 原子状态机与 CLI 测试、Linux P.25、Windows WP.21 均通过；提交 `1caada0`、CI 30271007552 |
| F8.8 `[done]` | detached 管道 EOF | `run -d`、`container start`、`start` 均由 Windows WP.22 持续验证；提交 `55761da`、CI 30272887266 |

### F9 对标能力补齐 `[planned]`

按 §2.4 的**跨象限收益**排序，不按实现难度排。每项都要能落到门禁上，
否则又会变成"README 宣传了但没人跑过"的那类条目（F4.3 的教训）。

```text
F9
├── F9.1 卷 / 绑定挂载 `-v host:guest[:ro]`   —— [partial] 仅 Linux 宿主；Windows 两格均明确拒绝
├── F9.2 端口映射 `-p`                        —— [done]（Linux 侧，仅 TCP）
├── F9.3 镜像构建（Dockerfile 子集）          —— [done]（Linux + Windows）
├── F9.4 Windows 文件系统写重定向             —— 单象限，且受 §2.4 天花板限制
├── F9.5 构建分层缓存                         —— [done]（Linux + Windows，WP.18）
├── F9.6 重启策略 `--restart`                 —— [done]（门禁 R.1–R.5）
├── F9.7 `--user UID[:GID]`                   —— [partial] 仅 Linux，只映射一个 id
├── F9.8 `--cap-add` / `--cap-drop`           —— [done]（仅 Linux，门禁 CAP.1–CAP.5）
├── F9.9 `--seccomp-deny`                     —— [partial] 拒绝名单，非 docker 的允许名单
├── F9.10 健康检查 `--health-cmd`             —— [done]（仅 Linux，门禁 HC.1–HC.5）
├── F9.11 `--network container:<NAME>`         —— [done]（仅 Linux，门禁 NC.1–NC.4）
├── F9.12 overlay 运行期可写层                 —— [done]（仅 Linux，门禁 OV.1–OV.8）
├── F9.13 `wbox push`                          —— [partial] 平铺单层，门禁 PSH.1–PSH.5
├── F9.14 compose 子集                         —— [partial] 仅 Linux，门禁 CMP.1–CMP.7
├── F9.15 IPC/UTS 隔离与共享                   —— [done]（仅 Linux，门禁 IU.1–IU.7）
├── F9.16 原始层留存与原样回推                 —— [done]（门禁 PSH.6–PSH.7）
├── F9.17 构建产物分层（基础层 + 增量层）      —— [done]（门禁 PSH.8a–PSH.8c）
├── F9.18 FROM 硬链接共享基础层                —— [done]（仅 Linux，门禁 OVB.1–OVB.4）
├── F9.19 `wbox diff` 列出容器改动             —— [done]（仅 Linux，门禁 DF.1–DF.3）
├── F9.20 `wbox commit` 固化容器改动           —— [done]（仅 Linux，门禁 CM.1–CM.4）
├── F9.21 `pause` / `unpause`                  —— [partial] 信号实现，非 freezer
├── F9.22 `save` / `load` 离线搬运镜像          —— [done]（门禁 SL.1–SL.6）
├── F9.23 `wbox cp` 宿主↔容器拷贝             —— [done]（仅 Linux，门禁 CP.1–CP.6）
├── F9.24 `wbox stats` 实时资源占用           —— [done]（仅 Linux，门禁 ST.1–ST.5）
├── F9.25 `export` / `import` 容器文件系统    —— [done]（export 仅 Linux，门禁 EX.1–EX.7）
├── F9.26 `wbox restart` 与可重启的 run -d    —— [done]（门禁 RT.1–RT.7）
├── F9.27 `wbox rename` / `wbox prune`        —— [done]（门禁 RN.1–RN.6）
├── F9.28 `logs -f` / `--tail`               —— [done]（门禁 LG.1–LG.4）
├── F9.29 `ps -q` / `rm -f`                  —— [done]（门禁 RMF.1–RMF.5）
├── F9.30 多容器名一致性与错误信息收敛      —— [done]（门禁 RMF.6–RMF.7）
├── F9.31 `images -q` / `rmi` 多引用        —— [done]（门禁 IMQ.1–IMQ.3）
├── F9.32 暂停状态可见（ps / inspect）      —— [done]（仅 Linux，门禁 PZ.4–PZ.5）
├── F9.33 inspect 如实反映挂载与端口       —— [done]（门禁 INS.1–INS.3）
├── F9.34 状态口径收敛与网络模式三态      —— [done]（门禁 INS.4–INS.5）
├── F9.35 命名卷（`wbox volume`）           —— [done]（门禁 VOL.1–VOL.6）
├── F9.36 `--entrypoint`/`--env-file`/四条 Dockerfile 指令 —— [done]（门禁 EP.1–EP.7）
├── F9.37 容器内工作目录（WorkingDir / `-w`）—— [done]（仅 Linux 镜像模式，门禁 WD.1–WD.5）
├── F9.38 `ADD` 与 `ps --filter`              —— [done]（门禁 AF.1–AF.7）
└── F9.39 多阶段构建                        —— [done]（门禁 MS.1–MS.5）
```

#### F9.1 卷 / 绑定挂载

**F9.1 卷 / 绑定挂载** `[partial]`（Linux 宿主已完成，门禁 V.1–V.4 + V.2b/V.2c）。
已定的语义：

- 只读/读写：`:ro` / `:rw`（默认读写）。**`:ro` 必须 remount 第二次才生效**
  ——首次 bind 会忽略 `MS_RDONLY`，这是 `mount(2)` 的既定行为；漏了这步
  `:ro` 会静默变成可写，那比不支持只读更糟。V.2 专盯这条。
- **而且只读必须递归**（V.2b/V.2c）。bind 用的是 `MS_BIND|MS_REC`（把源下面的
  子挂载一并带过来），而 remount 那步的 `MS_RDONLY` **只作用于顶层**——实测
  （内核 6.18）容器内 `/ro` 是 `ro`、`/ro/sub` 却是 `rw`，往子挂载写入 rc=0
  且文件真的落到宿主上。这是"给了一个没兑现的承诺"，比不支持只读更糟。
  现用 `mount_setattr(AT_RECURSIVE)` 一次把整棵挂载树设成只读；选它而不是
  遍历子挂载逐个 remount，是因为这段代码跑在 **fork 后的子进程**里、受
  async-signal-safety 约束（不能分配内存、不能读 `/proc`），而 `mount_setattr`
  是单条裸 syscall。老内核（`ENOSYS`）**只在等价时才回退**到非递归 remount
  ——仅当源下面确实没有子挂载，有子挂载却退回去就是静默地只兑现一半。
  **只认 `ENOSYS`、不认 `EINVAL`**：后者是我们自己参数写错，退回去就成了
  静默降级。V.2c 是反向判据（不加 `:ro` 时子挂载照常可写），防止一个
  "永远拒绝写"的实现也变绿。
- 宿主路径**必须已存在**，不自动创建：拼错的路径会变成一个空目录，用户直到
  发现数据"不见了"才知道挂错了（V.4）。容器内的挂载点则会自动建——它在
  rootfs 里，不是宿主。
- 挂载在 `pivot_root` **之前**完成：切根后旧根 detach，宿主源路径就没了。
- **拒绝 `-v <任意>:/`**（V.3）：挂到容器根等于把隔离作废。这不是防手滑，
  是防"一条命令让沙箱失效"。
- 宿主模式同样支持：虽不换根，但已在独立 mount namespace 里，bind 只对容器可见。

**Windows 原生程序侧**仍不支持且明确报错：AppContainer 无通用路径重定向，
完整 bind/写重定向需要 minifilter 驱动，撞 §2.4 天花板一。

**Windows OCI 侧必须由纯 Rust guest VFS 实现，不能提前放开 `-v`**：

1. Rust supervisor 持有宿主目录能力；Rust guest runtime 只接收 mount id、
   guest target、对象类型与 `read_only`，不得接收可逃逸的宿主绝对路径。
2. 路径解析逐组件拒绝 reparse/junction/symlink 越界，不得递归修改用户目录 ACL。
3. `:ro` 在 Rust VFS 的所有修改入口统一返回 `EROFS`；`:rw` 的修改实时回到宿主。
4. 首版只承诺目录 bind；文件 bind 在实现前明确拒绝。
5. 不得引入任何 native filesystem library 或 C broker client 来实现它——
   §2.2.1 两档口径都挡着。（原文写的是"不得通过修改 Blink……"：`vendor/blink`
   已删除，这条约束的**对象**没了，但**规矩**照旧适用于现在的
   `crates/wbox-linux`。历史上确实有过一次 brokerfs/C 实验，已撤回。）

验收必须证明 `:rw` 修改实时回到宿主，`:ro` 的每条写通道均失败且宿主元数据
不变；多卷、嵌套目标、`..`、绝对/相对 symlink、junction、dirfd 逃逸、detach、
`--rm`、stop、启动失败与 supervisor 强杀都不能泄漏句柄、状态目录或宿主权限。

#### F9.2 端口映射

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

#### F9.3 镜像构建

**F9.3 镜像构建** `[done]`（Linux 门禁 B.1–B.6；Windows 门禁 WP.18）。
`wbox build -t NAME[:TAG] [-f Dockerfile] <上下文>`，子集为
`FROM/RUN/COPY/ENV/WORKDIR/CMD/ENTRYPOINT`。

- **`RUN` 直接复用运行期的容器路径**（同一 backend、同一套 namespace 与限额），
  所以构建期与运行期隔离强度一致，不存在"构建时能做、运行时不能"的错位。
- Windows 使用 staging rootfs：`RUN` 经 AppContainer + 模拟器执行，临时 profile
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

#### F9.4 Windows 文件系统写重定向

**F9.4 Windows 文件系统写重定向** `[边界已定]`。W3 真机取证确认：不装驱动
无法获得 Sandboxie 的任意路径 copy-on-write，UAC VirtualStore 在 AppContainer
内也不生效。可用近似是默认拒绝、显式 ACL 授权，以及 Windows 自动映射的 package
私有 `LOCALAPPDATA`/临时目录；后两者由 WP.25/WP.27 持续覆盖。

#### F9.6 重启策略

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

#### F9.7 `--user UID[:GID]`

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

#### F9.8 `--cap-add` / `--cap-drop`

**F9.8 `--cap-add` / `--cap-drop`** `[done]`（门禁 CAP.1–CAP.5）。

与 F9.7 是同一条线的两半：`--user` 换的是**身份号**，`--cap-*` 改的才是
**权限面**。二者常被混为一谈——rootless 下进程在自己的 userns 里始终持有全部
capability（创建者身份使然），所以 `--user 1000` 并不降权，真要收窄只能动
capability 集合本身。

**这些 capability 管的是容器自己的命名空间**，不是宿主。userns 里的
`CAP_SYS_ADMIN` 换不来对宿主的任何权力，所以这不是"多加一道宿主防线"，
而是限制 guest 在它自己的沙箱内还能做什么。门禁 CAP.4 用行为而非位图取证：
丢掉 `SYS_ADMIN` 后容器内挂 tmpfs 被拒，而不带该选项时同一操作成功。

落地细节里有三处不做就等于没做的：

- **必须削 bounding set**，不能只清 `CapEff`。只清 effective 的话 execve
  之后还能重新拿回来。削 bounding set 又要求 effective 里还留着 `CAP_SETPCAP`，
  所以顺序是"先 `PR_CAPBSET_DROP` 循环，后 `capset`"，反了就只削掉一部分。
- **要清 ambient 集**，否则 execve 后它会把 capability 重新抬进 permitted。
- **编译期的名字表会落后于内核**。未写 `drop ALL` 时，表外位号一律保留，
  否则升级内核就会静默丢掉新 capability。要丢多少由运行时的
  `/proc/sys/kernel/cap_last_cap` 决定，不靠编译期常数。

默认**保留全部**，与既有行为一致。docker 的默认是一份精选白名单，那是它多年
生产经验的产物；照抄名单却不具备同等验证，只会造出"看着像 docker、行为不一样"
的坑。要收紧的人写 `--cap-drop ALL`，语义明确且可验证。非 Linux 宿主明确拒绝：
Windows 的 AppContainer 是另一套 SID 模型，逐条对应不了。

#### F9.9 `--seccomp-deny`

**F9.9 `--seccomp-deny`** `[partial]`（门禁 SEC.1–SEC.6）。

**为什么是拒绝名单而不是 docker 的允许名单**，这点比实现本身更要紧，否则用户
会以为拿到了 docker 级别的边界。docker 默认放行一份精选的约三百个 syscall、
其余一律拒绝；那份名单是它多年生产经验的产物。照抄一份自己没有同等验证的名单
会造出"看着像 docker、行为不一样"的坑——漏掉一个 syscall 就是一类程序起不来，
而且往往在很深的地方才炸。

**代价直说**：拒绝名单不是完备边界。没写进名单的一律放行，内核新增的 syscall
也自动放行。它兑现的是"这几个我明确不想让 guest 调的调不了"（挡 `ptrace`、
`mount`、`keyctl` 是运维上最常见的诉求），每一条都能验证；它不兑现
"未知的一律挡住"。

与 F9.8 互补，谁也替代不了谁：capability 管"有没有权限做某类事"，seccomp 管
"这个入口能不能进"。有些 syscall 不需要任何 capability（`ptrace` 同 uid 的进程
就是），只能靠 seccomp 拦。

几处不做就出错的细节：

- **必须比对 `seccomp_data.arch`**。同一个 x86-64 内核也接受 x32 ABI，两套 ABI
  的 syscall 号不同；不比对就会"按 x86-64 的号拦，换 x32 的号绕过"。架构不符
  一律 kill，不是放行。未适配的架构**明确拒绝**而不是静默不装——静默不装是最坏
  的结果，用户以为拦住了。
- **顺序排在 `--cap-drop` 之后**：过滤器可能把 `capset` 本身也拦了，反过来就会
  让 `--cap-drop` 静默失效。
- **装载前置 `PR_SET_NO_NEW_PRIVS`**：没有它，内核要求调用者持有 `CAP_SYS_ADMIN`
  才允许装过滤器，而我们恰恰可能刚被 `--cap-drop` 削掉。
- **拒绝 `execve`/`execveat` 是自毁配置**，在参数层就挡下。过滤器是在
  `execve` guest 之前装上的（装晚了 guest 已经在跑），且分不出"启动用的那次
  exec"与"guest 自己发起的 exec"，所以这个配置没有可用形态。不拦的话用户只会
  看到一句 "Operation not permitted"，看不出是自己配的。
- 名表**刻意不求全**，只收常被点名拦截的那些；写不出名字可以**直接写号**
  （`--seccomp-deny 165`）。号一律取自 `libc::SYS_*`，跨架构自动正确。

BPF 指令序列是纯函数、可离线断言（`filter_program_shape_and_jump_offsets`）：
seccomp 一装上就撤不掉，靠"跑一遍看看"调试代价太高，而跳转偏移算错的表现是
"配置了却没拦住"——最危险的失败形态。

#### F9.10 健康检查

**F9.10 健康检查** `[done]`（门禁 HC.1–HC.5）。`--health-cmd` 开启，
`--health-interval` / `--health-retries` / `--health-start-period` 调参，
默认值与 docker 一致（30 秒 / 3 次 / 0）。

**探针跑在容器里，不是宿主上。** 这条是整个功能的成立前提：探针经 `setns`
进容器的 user/mount/pid/net namespace，**复用 `wbox exec` 的同一条路径**
（`exec_in_namespaces_quiet`，只是把 stdio 丢掉）。图省事在宿主上跑的话，
探到的是宿主状态而用户以为探的是容器，比没有健康检查更危险。门禁 HC.3 专盯
这条，且判据能区分两者：`/home` 在宿主存在、在测试 rootfs 里不存在，探针若
跑在宿主上会报 healthy，跑在容器里必须报 unhealthy。

循环与 F9.6 同一取舍：挂在 **supervisor 自己**身上，不另起守护进程。
supervisor 一死健康检查随之停止，不会出现"容器早没了、探针还在报 healthy"
的僵尸状态；代价同样是 supervisor 崩溃时健康检查失效。

其余取舍：

- 状态写**单独的 `health` 文件**，不塞进 `meta.json`。理由是写入频率——
  meta.json 每次写都要取状态操作锁，让一个按 interval 触发的后台循环去争那把
  锁，是拿正确性换存储上的整洁。
- `ps` 把健康状态**并进"状态"列**（`running (healthy)`），不新增一列：
  没开健康检查的容器占多数，为它们凭空加一列空白不划算。
- 探针命令整条交给容器内的 `/bin/sh -c`，与 docker 的 `CMD-SHELL` 一致——
  健康检查十有八九要用管道或 `&&`，拆成 argv 是给用户添麻烦。
- `--health-interval 0` 与 `--health-retries 0` 明确拒绝：前者让探针变忙循环，
  后者让第一次失败就判死（docker 最小为 1）。调参选项**单独出现也报错**——
  没有 `--health-cmd` 就没有探针可跑，静默接受会让用户以为配好了。
- `--restart` 期间容器会短暂消失又回来，监控**以状态目录为准**而不是以某一代
  的 pid 为准；`container.pid` 还没写回来时跳过该轮，不当作探测失败。

#### F9.11 `--network container:<NAME>`

**F9.11 `--network container:<NAME>`** `[done]`（门禁 NC.1–NC.4）。加入既有
容器的网络，两容器经 localhost 互通——docker 同名模式的核心用途（sidecar）。

**为什么 user 和 net 必须一起加入**，这是 rootless 下的关键约束：netns 的
`setns` 要求在其**属主 userns** 内有 `CAP_SYS_ADMIN`。自建 userns 后与目标的
userns 互为兄弟，拿不到那个权限；先 `setns` 进目标 userns（同宿主 uid 即有
权限），才进得了它的 netns——与 `wbox exec` 是同一条已验证的路径。加入方随后
只 `unshare(NEWNS|NEWPID)`，身份映射沿用 peer 的（不能也不需要重写，重写会
EPERM）。

由此的组合约束都在参数层报错，理由是"每样东西只能有一个出处"：

- 与 `--allow-network`/`--network host` 冲突：网络来自 peer 或宿主，二选一。
- 与 `-p` 冲突：本容器没有自己的网络可发布，`-p` 应加在拥有网络的容器上。
- 与 `--user` 冲突：加入模式复用 peer 的 userns，uid 映射由它建立。

目标 namespace 的 fd 在 fork 前打开，fd 本身会钉住 namespace——peer 随后退出
也不会让加入方踩空；但目标**已退出**时明确拒绝（NC.4），静默起一个断网容器
会让 sidecar 悄悄失联。

**边界如实记**：这不是自定义 bridge。加入方与 peer 共享同一个 lo，端口空间
也是同一个；真正的多容器网络（每容器独立 IP、互相路由、内建 DNS）在 rootless
下需要 slirp4netns/pasta 级的常驻用户态网络栈，与"免安装、无服务"（§2.2）
冲突，列为不做。

#### F9.12 overlay 运行期可写层

**F9.12 overlay 运行期可写层** `[done]`（门禁 OV.1–OV.8）。

**修的是一个真实缺陷**：此前 Linux 镜像模式直接把共享镜像缓存当根，容器对 `/`
的写入落在 `~/.wbox/images/.../rootfs` 里，一个容器的垃圾会被之后所有同镜像
容器看到（R.5 调试时亲眼所见）。docker 语义是写入永不碰镜像。

做法：把 overlay 挂在 rootfs 路径**自身**之上（挂载时即持有底层 dentry 引用，
这是 overlayfs 的既定用法）。好处是所有既有路径——卷挂载目标、设备 bind、
`pivot_root`——一概不用改，挂上之后看到的自动是合并视图。挂载顺序在卷绑定
**之前**，否则卷会被 overlay 盖住。upper/work 放在容器状态目录 `layer/` 下：
随 `rm` 一并清理，而 detached 容器退出后还能翻 upper 看"它到底改了什么"——
白得的排障能力（OV.3）。

三处边界如实记：

- **必须带 `userxattr`**（内核 5.11+）。不加的话 overlay 要写
  `trusted.overlay.*` xattr 来标记 opaque 目录，而那需要**初始 userns** 里的
  `CAP_SYS_ADMIN`——rootless 拿不到。后果很具体：容器里 `rm -rf` 一个**镜像自带
  的目录**直接 `EIO` 失败，而删**文件**却是好的——所以只测文件根本发现不了。
  OV.6 专盯这条。这个缺陷是做 L5b 的可行性实验时才撞出来的。
- **内核 <5.11** 不允许 userns 内挂 overlayfs。父进程先用独立 userns 真挂一次
  探测（探测必须写 uid_map——光有 capability 不够，upper 的属主在未映射 ns 里
  是 overflow uid，过不了 overlayfs 属主校验；首次实现正是这么失败的）。不支持
  时**直接报错**：早先是打一句 stderr 警告然后照常启动，容器对 `/` 的写入就
  落进共享镜像缓存了——一句警告不改退出码，脚本里根本看不见，这既是隔离缺口
  也违反 §2.2「限制无法生效必须明确报错」。要共享写入语义的用
  `WBOX_NO_OVERLAY=1` 显式声明——**逃生口要用户自己按**。
- **启动过程本身也不能改动 lower**（门禁 OV.7/OV.8）。overlay 是子进程挂的，
  而父进程要在 `fork` 前把几个挂载点先建出来：`pivot_root` 的 put_old
  （`.wbox_oldroot`）、`/dev/*` 的 bind 目标、每个 `-v` 的目标目录。那时
  overlay 还没挂，早先那些 `create_dir_all` 全落进了共享镜像缓存——**跑一次容器
  就往镜像里塞一份**，下一个容器起来就看得到，`commit`/导出也捎带上。F9.12 花
  力气做的隔离，被启动路径自己从旁边绕过去了。现在挂载点一律建在 upper 里，
  子进程仍按 rootfs 路径挂载（合并视图下是同一个路径）。两处配套：符号链接
  检查改成按**合并视图**逐段做（upper 有同名项以 upper 为准，否则看 lower），
  否则上一次运行在 upper 里留下的链接会被漏掉；以及 upper 里的 **whiteout**
  （`0:0` 字符设备）要在建挂载点前抹掉，不然第二次 `start` 直接 `EEXIST`。
  whiteout 的来源有二：**本改动之前建的镜像缓存**里已经有 `.wbox_oldroot`，
  子进程 `pivot_root` 后 `rmdir` 它就在 upper 留下标记（实测就是这么撞上的）；
  以及容器自己删掉了某个镜像自带、又正好是下次挂载点的目录。这条只有"同一个
  容器跑第二次"才走得到，一次性用例发现不了（OV.8 专盯它）。
- **build 的 `RUN` 步骤豁免**（`direct_rootfs_writes`）：它的写入就是构建产物，
  引到 upper 等于把 RUN 效果全丢掉。F9.12 保护的是**运行期**的共享缓存。
- 路径含 `,` 或 `:` 时 overlayfs 选项串无法表达，出声回退。
- 这与"镜像分层存储"是两件事：`FROM`/pull 仍整份复制 rootfs，那是另一格。

#### F9.13 `wbox push`

**F9.13 `wbox push`** `[partial]`（门禁 PSH.1–PSH.5）。

**推出去的是平铺单层**，这点必须先说清，命令本身也会在开始时打印。本地缓存存
的是**解包后的 rootfs**，pull 时层 tar 解开就丢了——没有原始 blob 可以原样回推。
所以做法是把整个 `rootfs/` 重新打成一个 `tar.gz` 层，配单层 manifest + config
再推，语义等价于 `docker commit` 后 push：内容一致、**分层历史不保留**。
指望推上去还能与上游共享层的话做不到。要改这一点得让缓存额外保存原始压缩层，
是存储布局的改动，牵动 pull/build/overlay 三条路径，属"镜像分层存储"那一格。

实现上几处不做就出错的：

- **两个 digest 不能混**：`diff_id` 是未压缩 tar 的（config 的 `rootfs.diff_ids`
  要它），layer descriptor 要的是压缩后的。混了 registry 校验能过，但拉回来
  diff_id 对不上。
- **打包要排序**：`read_dir` 顺序不保证，不排序则内容没变也每次算出新 digest，
  `HEAD` 判存在的跳过就永远命不中。
- **符号链接不跟随**：跟随会展开成内容副本，既撑大体积又丢语义（busybox 的
  applet 链接全废）。
- **先传 blob 后传 manifest**：顺序反了符合规范的 registry 会以
  `MANIFEST_BLOB_UNKNOWN` 拒绝。
- **`history` 清空**：留着旧 history 与"只有一层"自相矛盾。
- `Location` 补成绝对 URL 时**校验同 host**：registry 可以返回绝对 URL，指向
  别处就是把待上传内容导给第三方，与 realm 的同 host 约束（H3）同一条规矩。

`WBOX_INSECURE_REGISTRY=<host>` 是明文 http 的逃生口（本地 registry / 门禁
stub 用；docker 的 `--insecure-registry` 同理）。两条硬约束：必须**精确匹配**
host（不做前缀/通配），且明文信道上**绝不发送凭证**——Basic 分支要求 https，
因此走不到。默认永远 https（PSH.1 盯这条）。

门禁不打真 registry：用 python3 起最小 stub，`PUT manifest` 时自检引用的 blob
是否都已入库（PSH.2），再 `wbox pull` 从 stub 拉回来比对内容、符号链接与
config（PSH.4）。只断言"push 返回 0"证明不了推上去的东西能被拉回来用。

#### F9.14 compose 子集

**F9.14 compose 子集** `[partial]`（门禁 CMP.1–CMP.7）。范围先钉死，不做
"重新实现 docker-compose"：八个 service 字段（`image`/`command`/`volumes`/
`ports`/`environment`/`depends_on`/`restart`/`healthcheck`）+ 三个动词
（`up -d`/`down`/`ps`）。**其余字段一律明确报错**——静默忽略 `build:`/`networks:`
会让用户以为配置生效了，对编排文件这是最危险的失败形态（CMP.5）。

**复用而不是另起一套**：`up` 把每个 service 翻译成一条 `wbox run` 的 argv 交给
`cmd_run`，`down` 走 `stop` + `rm`。于是 compose 天然继承 run 的全部校验与语义，
不会出现"compose 起的容器和 run 起的行为不一样"这种最难查的偏差。

**网络语义与 docker 不同，直说**：docker compose 给每个项目建 bridge + 内建
DNS，服务之间用服务名互访。rootless 下没有 bridge（§2.4 已记：那需要
slirp4netns 级常驻网络栈，与"免安装、无服务"冲突）。这里是**第一个服务持有
网络，其余用 `--network container:` 加入它**，服务间经 `localhost` 互通。
对 sidecar 这类主要场景等价；"用服务名当主机名"做不到。CMP.2 三方比对
netns inode 取证。

其余取舍：

- **YAML 是手写的有界子集**，不引 `serde_yaml`（已归档停维护，不再收安全修复）。
  不支持的构造（锚点/别名、多行标量、流式映射、tab、序列里的映射）**逐条报错
  并带行号**——子集解析器最危险的失败是"看着解析成功了其实理解错了"。
  检查要看**值**的位置而非行首：`a: &anchor` 与 `a: {b: 1}` 的行首都是普通字符。
- `depends_on` 只定**启动顺序**，不做 `condition: service_healthy`（那要在启动
  中等待另一容器变健康，是另一档复杂度）。循环依赖点名报错（CMP.6）。
- `up` **要求 `-d`**：多服务同时前台运行没有确定的 stdio 归属。明说而不是偷偷
  加 `-d`——偷偷加会让用户以为命令挂了（CMP.7）。
- `environment` 的序列与映射两种写法归一成 `K=V`，下游只认一种形状。
- `healthcheck.test` 的 `CMD`/`CMD-SHELL` 前缀都归一成一条 shell 命令
  （wbox 的 `--health-cmd` 本就交给 `/bin/sh -c`）；`NONE` 明确报错——它的语义
  是"禁用继承来的探针"，而 wbox 不存在继承这回事。

#### F9.15 IPC/UTS 隔离与共享

**F9.15 IPC/UTS 隔离与共享** `[done]`（门禁 IU.1–IU.7）。

**先修的是一个隔离缺口，不是加特性。** 此前 wbox 只 unshare
user/mount/PID(/net)，容器**直接共享宿主的 IPC 与 UTS**：guest 能看见并操作
宿主的 System V 信号量与共享内存段，也能 `sethostname` 改掉宿主主机名。
docker/podman 默认隔离这两个。IU.1/IU.2 的判据因此是"与宿主 inode 不同"
与"宿主主机名不变"。

主机名默认取**容器名**（docker 用容器 ID 前 12 位，我们用名字更可读），
`--hostname` 可覆盖；超长时截断而不是失败——一个太长的容器名不该让容器起不来。

`--ipc`/`--uts container:<NAME>` 与 `--network container:`（F9.11）**共用同一套
机制**：`JoinNs` 从"user+net 两个写死的 fd"改成"user + 一组按需打开的 fd"。
各写一份迟早会在"必须先进 userns"这条关键顺序上漂移，而那条顺序错了就是
`EPERM`。三者若指向不同容器则明确报错：加入模式要先进目标的 user namespace，
而一个进程只能在一个 userns 里，挑一个赢家等于悄悄忽略另一个（IU.7）。

`--ipc`/`--uts` 目前**只收 `container:<NAME>`**。docker 还有 `host`/`shareable`/
`private`：`private` 已是默认，而 `host` 等于放弃刚补上的隔离——要开必须是显式
且有理由的设计，不顺手加。

**连带修掉一处不一致**：`exec` 此前只 setns 进 user/mnt/net/pid，IPC/UTS 一隔离，
`wbox exec` 就会落在**宿主的**那两个里——用户以为自己在容器内，看到的却是宿主
主机名、碰得到宿主的 System V 对象。IU.6 盯这条。这个缺陷是写门禁时才暴露的：
第一版判据用 `exec` 读 ns inode，量出来"共享没生效"，查下去才发现错的是探针
所在的位置，不是共享本身。

#### F9.16 原始层留存与原样回推

**F9.16 原始层留存与原样回推** `[done]`（门禁 PSH.6–PSH.7）。

pull 时**除了解包，还原样留一份压缩层**（`blobs/<digest>`）。多花一份磁盘换来
解包结果给不了的两件事：`push` 能原样回推、将来 `FROM` 能复用基础层。

原样回推的判定很硬：本地 manifest 必须是真的（不是 build 写的 `{}` 占位），
它引用的每个层 blob 都还在，且 `config.json` 的 digest 与 manifest 里写的一致
（不一致说明缓存被动过，那时回推会推出一个自相矛盾的镜像）。任一条不满足就
**退回 flatten 并打印原因**——不静默改变语义。满足时推上去的 manifest 字节与
拉下来时一字不差，故 **digest 不变、层与上游共享**，这正是 §4.9 L5 写死的判据
（PSH.7 直接比对 stub 侧记录的原始与推入 digest，并断言层数仍为 2）。

registry 回报的 digest 若与本地算的不同，说明它改写了 manifest，"原样"就没兑现
——这时**出声警告**，不能让用户以为分层还在。

**还没做的那半如实记**：`FROM` 仍整份复制 rootfs，没有改成引用基础层。存储侧
已经就绪（原始层都在），剩下的是 build 侧的改动，见 §4.9 L5b。

#### F9.17 构建产物分层

**F9.17 构建产物分层** `[done]`（门禁 PSH.8a–PSH.8c）。

`build` 的产物不再写空 manifest，而是写成**基础镜像的层 + 一个增量层**。
于是 push 一个派生镜像时，基础层被 registry 的 `HEAD` 判定已存在而**跳过上传**，
只传增量——这正是 §4.9 L5b 判据的后半条（PSH.8b）。

增量层按**内容**比对基础 rootfs 与构建产物得出，不看 mtime：构建过程会刷新
mtime，看 mtime 会把整棵树都判成改过，增量层就退化成全量。删除写成 `.wh.<name>`
whiteout，与解包侧已有的约定一致，故拉回来能正确叠加（PSH.8c 三层内容逐个核对
——分层推对了但内容错了，比不分层更糟）。

做不到分层时（基础镜像是旧缓存、或它本身就是 build 产物）**退回写空 manifest**，
push 据此走 flatten（F9.13）。分层失败一律不致命：构建本身已经成功，分层只是让
后续 push 更省，不该因为它出问题就让整个 build 失败。

`push` 的输出相应改成如实报告：`-V` 下逐个说"上传"还是"跳过（registry 已有）"，
并默认打印跳过/实传的层数。此前一律说"上传"——那把分层复用省下的流量说没了。

**磁盘那半没做，原因是文件系统**：`FROM` 仍整份复制 rootfs。要省磁盘只有两条路，
本机都走不通或不可验证——reflink（`cp --reflink`）在 ext4 上不支持（已实测
`Operation not supported`）；hardlink 则会让 `RUN` 就地写坏共享的基础镜像缓存，
除非把 build 的写入模型改成 overlay + 合并（含 whiteout 处理），那是另一档改动。
详见 §4.9 L5b。

#### F9.18 `FROM` 硬链接共享基础层

**F9.18 `FROM` 硬链接共享基础层** `[done]`（门禁 OVB.1–OVB.4）。

`FROM` 不再按字节整份复制基础 rootfs，改为**硬链接**：两个派生镜像与基础镜像
共享同一批数据块。实测合计占用 1432KB，而两份之和是 2788KB。

**共享的前提是此后没有任何就地改写**——这条纪律一破就会写坏别的镜像，是最不该
引入的一类缺陷。三条写入路径逐个封死：

- `COPY`：先 `unlink` 再落盘（`fs::copy` 会 truncate 后就地写，直接改到共享 inode）。
- `RUN`：走 overlay，写入落在**本步专属的 upper** 里，跑完再合并回 staging。
  合并时同样先 `unlink`，只对改动过的文件断开硬链接。
- 合并本身：whiteout（字符设备 0:0）删掉目标；opaque 目录（`user.overlay.opaque`）
  先清空再递归。这两种形态与 tar 层的 `.wh.` 前缀**不是**一套，判别代码是独立的。

overlay 不可用（内核 <5.11）或 `WBOX_NO_OVERLAY=1` 时**退回整份复制**——
省磁盘是优化，正确性不能让。

OVB.1 直接对基础镜像 rootfs 取全文件 sha256 摘要，构建前后必须一致；OVB.4 比对
被 `RUN` 改写文件的 inode，必须与基础镜像的不同。只测"产物内容对"是不够的——
产物对而基础镜像被改坏，恰恰是这个设计最危险的失败形态。

#### F9.19 `wbox diff`

**F9.19 `wbox diff`** `[done]`（门禁 DF.1–DF.3）。列出容器相对镜像改动了什么，
输出格式与 `docker diff` 一致（`A`/`C`/`D` + 容器内路径）。

**这一格几乎是 F9.12 白送的**：容器对 `/` 的所有写入都落在 overlay 的 upper 里，
**upper 的内容就是答案**，不必扫描整棵 rootfs 去比对——那既慢又要拿到镜像原树。
判别规则与 build 侧的合并逻辑同源（字符设备 0:0 = 删除），两处不会各自漂移。

两个容易做错的地方：

- **镜像里已有的目录不报**。overlay 会为"改了某个子项"而在 upper 里建同名目录，
  把它们都报成 `C` 会让输出塞满噪音。只有目录**本身是新增的**才报 `A`。
- **没有 overlay 层时报错，不打印空清单**。宿主程序模式不换根、内核不支持时
  回退了共享写入，这两种情况都拿不出这个答案；空清单会被读成"没改过"，
  那是把"不知道"说成了"没有"（DF.3）。

`.wbox_oldroot` 是 pivot_root 的暂存目录、必然出现在 upper 里，已滤掉——
把自己的实现细节混进用户看的答案里，等于让用户替我们记住内部约定（DF.2）。

#### F9.20 `wbox commit`

**F9.20 `wbox commit`** `[done]`（门禁 CM.1–CM.4）。把容器的改动固化成新镜像。

**整条链路都是复用，没有一件新机制**——这一格能成立，是因为前面几格把机制建对了：

- 基础 rootfs 用 `link_tree` 硬链接铺开（F9.18），磁盘不翻倍（实测合计 1452KB
  vs 两份之和 2808KB）；
- 容器的改动就在 overlay upper 里，用 `merge_overlay_upper` 合并进去（F9.18 的
  同一份逻辑，whiteout / opaque 一并处理）；
- 元数据走 `write_layered_manifest`（F9.17），于是 commit 出来的镜像 push 时
  基础层同样会被 `HEAD` 跳过。

判据分两半，缺一不可：改动真的固化（新增/改动/**删除**三类都测，CM.1），
以及**基础镜像逐字节不变**（CM.2）。后半条是这条链路最危险的失败形态——
产物看着对而基础镜像被改坏，破坏的是别的镜像且很晚才会发现。

两处明确拒绝：没有 overlay 层时报错而不是 commit 一份与镜像相同的副本
（那会让用户以为改动固化了）；commit 目标与容器自己的基础镜像相同时拒绝，
否则会在铺硬链接的中途把 lower 抽掉。

不支持"省略镜像名自动生成"。docker 会给个随机 ID，而 wbox 的缓存按名寻址，
随机名只会留下一堆没人认领的镜像。

#### F9.21 `pause` / `unpause`

**F9.21 `pause` / `unpause`** `[partial]`（门禁 PZ.1–PZ.3）。

**用信号而不是 cgroup freezer，理由是可用性**：容器的 cgroup 只在设了
`--memory`/`--cpu-pct`/`--max-procs` 时才存在（见 `linux_limits.rs`），
没设限额就没有可冻的组。让 `pause` 时灵时不灵，比换一种实现更糟。
所以对容器内**每个**进程发 `SIGSTOP`，`unpause` 发 `SIGCONT`；进程清单
复用 `top` 的同一份枚举（`container_pids`）——一处另写一份的话，迟早出现
"top 看得见的进程 pause 漏了"这种最难查的偏差。

**代价直说，不假装等价**：

- `SIGSTOP` 进程**能观察到**（`SIGCONT` 后 `wait` 会返回，有些程序会重试或
  报错），freezer 则对进程透明；
- 停的是发信号那一刻已存在的进程；其后新 fork 的不受影响——不过父进程已停，
  正常情况下不会再有新进程。**副作用**：逃掉的那个子进程退出后没人回收
  （父进程停着），会留下僵尸。判「是不是暂停了」因此**只看容器 init 的状态**，
  不能要求整棵树每个进程都是 `T`——否则那个僵尸会让容器永远显示成 running
  （F9.34 修的就是这个）。
- **`wbox exec` 进一个暂停中的容器会成功，docker 那里会挂住**。原因是 exec 起的
  是一个**新**进程，它没收到过 SIGSTOP；而 docker 用 cgroup freezer，新进程一进
  该 cgroup 就同样被冻住。这条差异实测确认过，写在这里而不是假装等价——
  需要「暂停即完全冻结」的场景应当知道 wbox 给不了。

对"临时腾出 CPU / 挂起长任务容器"这类主要用途，这些差异无关紧要；
依赖信号语义的 guest 则可能受影响，故标为 `部分` 而不是 `有`。

判据是**行为**而非返回码：容器不停往宿主可见的文件写计数，pause 后计数必须
冻住、unpause 后必须重新增长（PZ.1/PZ.2）。只断言"pause 返回 0"证明不了任何事
——信号发出去了不等于进程真停了。

#### F9.22 `save` / `load`

**F9.22 `save` / `load`** `[done]`（门禁 SL.1–SL.6）。把镜像打包成 tar 搬到别处。

**为什么值得做**：目标用户里有"受管 Windows 机器上的开发者"（§3.1），那种环境
常常连不上 registry 或出网要审批。用一个文件搬镜像，不必架 registry。

**打的是整个缓存目录，不是 rootfs**：归档含 `rootfs/`、三个 json，以及 F9.16
留下的 `blobs/`（原始压缩层）。带上 blobs，搬过去的镜像才仍然能**原样 push**
（digest 不变）；只打 rootfs 的话，load 出来的会退化成"只能 flatten 推送"。

归档里写一个 `wbox-image.json` 记录镜像身份，load 端不必靠文件名或调用方记忆
去猜这是哪个镜像；`-t` 可覆盖落地名。

**load 的安全约束**：tar 是外部输入，路径穿越是这里唯一真正危险的东西。
除了逐条拒绝绝对路径与 `..`，还**按白名单**限定顶层条目（只收本模块自己写出去
的那几个名字）——与其事后判断"这个路径安全吗"，不如一开始就只认识我们自己产的
结构。非 wbox 归档明确报错并指出 `-t` 这个逃生口（SL.5）。

`save` **不默认写 stdout**（docker 会）。镜像动辄几十 MB，误写进终端是灾难性的
体验，而那个默认在管道场景之外几乎只会伤人，故要求显式 `-o`（SL.6）。

#### F9.23 `wbox cp`

**F9.23 `wbox cp`** `[done]`（仅 Linux，门禁 CP.1–CP.6）。宿主与容器之间双向拷贝。

**不进容器，走分层视图**。docker 的 `cp` 要求容器存在但可以已停止；wbox 能给出
同样的性质，而且实现更简单——F9.12 已经把容器的文件系统摊成了两层：上层是该
容器的 `layer/upper`，下层是共享只读的镜像 rootfs。宿主侧按"读先看 upper 再看
lower、写只写 upper"就得到与容器内完全一致的视图，**不需要 `setns`**。

由此白捡两个性质：

- **容器不必在运行**。已退出但没 `rm` 的容器照样能取文件——排查失败容器时这恰恰
  是最需要的场景，而 `setns` 方案在那时已经无从进入。
- **写进去的对运行中的容器立即可见**（CP.3）。overlay 是活的挂载，不是快照。

**必须认 whiteout**（CP.2）。容器删掉的文件在 upper 里是字符设备 0:0。不认它就会
把镜像下层那份"早已被删掉的"旧文件拷出去，而用户以为拿到的是容器现状——这是本格
唯一会**静默给出错误答案**的失败形态，所以判据要求它真的失败且不落盘。

**写只落 upper**（CP.4）。镜像目录被多个容器以硬链接共享（F9.18），写穿到下层会
污染别的容器。判据是 cp 前后基础镜像缓存的逐字节摘要不变。

**容器端靠"是不是已登记的容器"来认，不是靠含冒号**：`./a:b` 是合法的宿主文件名。
路径解析复用 build 的 `resolve_rootfs_path`——`..` 逃逸校验一处写、两处用，
分头写两份迟早有一份漏掉。

#### F9.24 `wbox stats`

**F9.24 `wbox stats`** `[done]`（仅 Linux，门禁 ST.1–ST.5）。容器的实时资源占用。

**不能照抄 docker 一律读 cgroup**。wbox 的容器 cgroup **只在设了限额时才存在**
（`linux_limits.rs`：没有 `--memory`/`--cpu-pct`/`--max-procs` 就不建组）。
照抄的话，`stats` 对一半的容器直接没有答案。所以两条路按可得性挑：有专属 cgroup
就读 `memory.current`/`cpu.stat`/`pids.current`（内核记账，精确）；没有就按 `top`
的同一份进程枚举去 `/proc` 逐个累加。

**"有专属 cgroup"的判据是"它和 wbox 自己的组不同"**。没设限额时容器就待在 wbox
所在的组里，那个组的数字含 supervisor 乃至整个会话——把它当成容器占用是错的，
而且错得看不出来（数字很像真的）。

**两种来源的精度不同，必须在输出里标出来**（ST.3）。`/proc` 那条路的内存是
**RSS 合计**，多个进程共享的页会重复计入；印成和 cgroup 一个样子是在诱导误读，
所以带 `proc*` 标记并附一行说明。

**CPU% 必须采两次**：`/proc` 和 `cpu.stat` 给的都是累计用时，单次采样只能算出
"从启动到现在的平均值"，那不是这条命令要回答的问题。分母用**真实经过的时间**
而不是标称的采样窗口（sleep 不精确，用标称值会系统性偏差）；上限不封在 100，
用满两个核就该显示 200%，与 docker 一致。

**判据是区分能力，不是返回码**（ST.1）：一个死循环烧 CPU、一个纯 `sleep`，
stats 必须把两者分开。只断言 `rc=0` 的话，一个恒返回 0.00% 的实现也能过。
已退出的容器要**报错**而不是打印一行 0（ST.5）——一行 0 会被读成"这个容器很闲"。

#### F9.25 `wbox export` / `import`

**F9.25 `wbox export` / `import`** `[done]`（export 仅 Linux，门禁 EX.1–EX.7）。
容器文件系统的裸 tar 搬运。

**先分清它和 `save`/`load`（F9.22）不是一回事**——docker 的用户也常搞混，所以
两边的错误信息都写明白：

| | 搬的是什么 | 带不带历史/配置 |
|---|---|---|
| `save` / `load` | **镜像** | 带：manifest、config、原始压缩层，load 回来还能原样 push |
| `export` / `import` | **容器的当前文件系统** | 不带：一棵 rootfs 压成 tar，层历史被压平 |

`export` 的用途是"把这个容器现在的样子交出去"——交给不装 wbox 的人，或塞进别的
构建流程，要的正是没有 wbox 特有结构的裸 tar（EX.3 盯这条）。

**实现上几乎没有新东西**：容器的完整文件系统 = 镜像下层 + overlay upper 合并，
而这件事 `ContainerLayers::materialize` 已经做了（`commit` 用的是同一个）。
所以 export = 物化到暂存目录 + 打 tar。**不**在打包侧边遍历分层边写——那要把
whiteout/opaque 的合并规则再实现一遍，两份规则迟早不一致。暂存目录无论成败都要
收拾（EX.4）：留着会在容器状态目录里悄悄多吃一份完整 rootfs。

**import 的安全约束比 load 更强**。`load` 收的是 wbox 自己产的结构，可以按白名单
认顶层；`import` 收的是任意来源的 rootfs tar，顶层是 `bin`/`etc`/`usr` 里的哪些
由归档决定，**无从白名单化**。能守住的只有两条——不许绝对路径、不许 `..`——
加上一律拼到 `rootfs/` 之下。两条合起来，归档里的任何路径都拼不出 `rootfs/`
以外的位置。EX.7 用真造出来的穿越归档取证，不是只测解析函数。

**单条目解包失败不中止整个归档**：裸 rootfs 里常有本机建不出来的条目（设备节点
要 root、属主可能不存在）。中止的话绝大多数 rootfs 都 import 不进来——那才是真正
无用的行为。但一个条目都没成功则报错，并指出带身份的归档该用 `load`。

**如实写空的 entrypoint/cmd**：裸 rootfs 里确实没有这些信息，编一个 `/bin/sh`
进去会让 `wbox run <镜像>` 表现得像是镜像自带的默认命令，而它其实是 wbox 猜的。

#### F9.26 `wbox restart`

**F9.26 `wbox restart`** `[done]`（门禁 RT.1–RT.7）。停掉再按原配置起来。

**做这一格时发现了一个真实缺口**：此前只有 `wbox create` 的容器记得住启动配置，
`run -d` 起的容器一退出就再也拉不回来——连 `wbox start` 都不行，而 docker 两种
都能 `start`/`restart`。补法是在 `run -d` 的父进程里落一份 `run-args.json`
（`runstate::save_run_args`）。

**它和 `create.json` 必须是两个文件，不能合并**：`create.json` 的存在本身就是
「这个名字被 create 占用了、该走 start」的标记（见 `reserve_detached`），而
`run -d` 起的容器并不处于那个状态；共用一个文件会让 `run -d` 之后再 `run -d`
同名容器被误判成「已有 create 配置」。两份文件格式相同，由同一个写入函数产出，
权限都收到 0600——argv 里可能有 `-e TOKEN=...`，它不该比原命令行更容易被同机
别的用户读到。

**restart 本身是纯编排**：`stop` 与 `start` 各自已经把难的部分做完了（前者停的
是 supervisor 而不是 guest、先礼后兵，后者在操作锁内原子领取配置）。不另写一条
启停路径——另写的那条迟早与它们出现行为差异，而这种差异最难查。stop 只多了一个
「要不要打印容器名」的开关：`restart` 自己按批次汇报，让 stop 也打一遍会变成同一个
名字出现两次（RT.3 盯这条）。

已退出的容器照样能 `restart`（docker 语义）：用户要的是「让它按原配置重新跑起来」，
它当前是不是在跑属于实现细节。

**判据是「换了一条命 + 配置照旧」**（RT.1/RT.2）：容器 PID 必须变，且 `run` 时给的
`--hostname` 重启后仍生效。只断言 `rc=0` 的话，一个什么都不做的实现也能过。

#### F9.27 `wbox rename` / `wbox prune`

**F9.27 `wbox rename` / `wbox prune`** `[done]`（门禁 RN.1–RN.6）。记录的改名与批量清理。

**`rename` 只对没在运行的容器开放，这不是偷懒**。运行中的容器把自己的名字用在
三处：overlay 可写层的路径（`dir_for(name)/layer`）、默认主机名、以及 Windows 上
那个按名字创建的 Job object。改名只动状态目录，改不到已经跑起来的 supervisor
手里的那几样，结果是容器名与它实际在用的资源对不上——一种改完看着成功、之后才
出问题的状态。宁可明说不支持，并在错误里讲清楚为什么（RN.3）。

改名要**连 `meta.json` 里的名字一起改**（RN.1）：只改目录的话 `ps` 还显示旧名，
目录名与记录名各说各话，比不支持改名更让人困惑。日志随状态目录整体搬走，
改名后 `wbox logs` 照样读得到（RN.2）——重建记录的做法会把日志丢掉，而 detach
容器的日志正是保留记录的全部理由。

**`prune` 默认不删**（RN.5）。删除不可逆而它一次删一批，所以先列清单、要求 `-f`
确认。不做交互式 y/N：wbox 常在脚本和 CI 里跑，读 stdin 会挂住；"两步走"在这两种
场景下都成立。预演返回 0 而不是非零——那是一次成功的预演，不是失败，否则
`wbox prune || echo 出错` 会误报。

**`created` 不在清理范围内**：那是用户 `wbox create` 特意留着待 `start` 的配置，
不是残渣，扫掉会让人白丢参数（docker 的 `container prune` 同样只清已停止的）。
判活取自 `runstate::liveness`，与 `ps` 同一份，不另写一套。

#### F9.28 `wbox logs -f` / `--tail`

**F9.28 `wbox logs -f` / `--tail`** `[done]`（门禁 LG.1–LG.4）。跟随与截取日志。

**跟随必须认日志被截断**。日志有体积上限，超了会被截断到 0 再从头写
（`runstate::enforce_log_cap`）。跟随时若只记一个「读到哪了」的偏移量，
截断之后文件长度小于该偏移，于是再也读不到新内容——**命令看着还在跟，其实已经
哑了**。这是这条链路上唯一会静默降级的失败形态，所以显式检测「文件变短了」并把
偏移归零，单测直接构造截断场景钉住。

**循环里先判活、再读一次**，顺序不能反：反过来会丢掉容器在「读完」与「判活」
之间写下的最后一段输出——而那一段往往正是失败原因（LG.3 单盯末行）。

**容器退出后跟随要自行结束**（LG.4），不能挂住等一个永远不会来的写入。

`--tail N` 不把结尾的换行当成一整行，否则 `--tail 1` 只会输出一个空行；
`--tail all` 按 docker 的写法等价于不限行数。

**门禁判据要求它「确实等了」**（LG.2）：只数行数的话，若容器早已跑完，一次性读全
也能凑够行数——那证明不了跟随。所以同时断言命令自身耗时明显大于 0。

#### F9.29 `wbox ps -q` / `wbox rm -f`

**F9.29 `wbox ps -q` / `wbox rm -f`** `[done]`（门禁 RMF.1–RMF.5）。补齐清场惯用法。

这两个开关单独看都小，但缺了任何一个，`wbox rm -f $(wbox ps -aq)` 这条 docker
用户天天用的清场写法就不成立——所以一起补。

**`-q` 只出名字，别的一律不出**（RMF.1）。它是给脚本用的，空表时那句人类友好的
说明、以及表头，都会被命令替换当成容器名传下去。wbox 的容器身份就是名字
（没有单独的 ID），所以 `-q` 输出名字而不是 docker 那样的 ID。
`-aq` / `-qa` 也认——那是肌肉记忆，不认会让人以为命令坏了；但**不做**通用的短
选项合并：其余组合目前没有意义，装作支持会在以后加新短选项时留下模糊语义。

**`-f` 必须显式要求**。删记录和杀进程是两件危险程度差很多的事：默认把后者也做了，
一次手误就是打断一个还在干活的容器。不带 `-f` 的拒绝行为原样保留（RMF.3）——
那是「`rm` 不会替你停容器」这条约定的全部意义。停的那一步直接复用 `stop` 那条路
（先礼后兵、Windows 的 Job 处理都在里面），不另写一份。

**判据要求进程树真的没了**（RMF.4），不只是记录没了：只删记录不停进程会留下没人
管得到的孤儿，而 `ps` 从此看不见它——正是不带 `-f` 时要拒绝的那种局面。

#### F9.30 多容器名一致性

**F9.30 多容器名一致性** `[done]`（门禁 RMF.6–RMF.7）。

`kill`/`rm`/`start`/`wait`/`restart` 早就收多个容器名，唯独 `stop`/`pause`/
`unpause` 只收一个。**这是纯粹的不一致**：同一批容器能一起 `kill` 却不能一起
`stop`，用户没法从别的命令推断出这条例外。三个都改成收多个，走 `args::each_named`
那条共用路径（一个失败不中断后面的）。

顺带修了两处这次才暴露出来的错误信息问题：

- **单个容器失败时不套汇总**。`each_named` 原本一律返回「N 个容器未成功（共 M 个）」，
  可当 M=1 时这句话毫无信息量，真正的原因被挤到 stderr 上另起一行——用户看到的
  主错误什么都没说。改成：只有一个名字时把原始错误原样返回。
- **共用措辞里不能塞某个调用方专属的动词**。`runstate::already_exited` 写死了
  「无法 exec」，于是 `wbox pause` 一个已退出的容器会得到「已退出，无法 exec」
  ——答非所问。改成由调用方传入它要做的那件事。

#### F9.31 `wbox images -q` / `rmi` 多引用

**F9.31 `wbox images -q` / `rmi` 多引用** `[done]`（门禁 IMQ.1–IMQ.3）。

做 `-q` 时先撞上一个**既有缺陷**：`wbox images` 的 IMAGE 列印的是**缓存目录名**
（`library_lbetest`），而缓存目录名是 `ImageRef::cache_name` 把 `/` 扁平成 `_`
之后的产物。用户照抄它去 `wbox rmi library_lbetest`，得到的是
「镜像 'library/library_lbetest' 未 pull」——**列出来的名字喂不回给任何命令**，
而 `images` 本身表面上是成功的。

修法是从缓存目录三元组**还原**出可用引用。还原本身有歧义（`_` 既可能来自 `/`
的扁平化，也可能本来就在仓库名里），但这里的做法是可证的：还原出候选引用后
**再解析回去、算一遍缓存目录**，只有算出来与实际目录一致才采用。既然 `rmi`/`run`
也都经同一个 `image_dir` 去定位，能这样往返的引用就一定指向这个目录——歧义留下的
多个候选，操作上等价。往返对不上时（缓存被手工改过、跨版本布局变化）如实标注
「缓存目录名，引用无法还原」，而不是给一个用不了的引用。

顺带把目录遍历从打印函数里拆出来（`oci::list_refs`）：此前枚举逻辑焊死在
`list()` 里，`-q` 想复用只能复制一份。`rmi` 同时改成收多个引用，走容器侧
同一条批处理路径（`args::each_named`），于是 `wbox rmi $(wbox images -q)` 成立。

#### F9.32 暂停状态可见

**F9.32 暂停状态可见** `[done]`（仅 Linux，门禁 PZ.4–PZ.5）。

F9.21 的 `pause` 一直是真停住的（PZ.1 用计数冻结证过），**但没有任何地方反映它**：
`wbox ps` 照旧显示 `running`，`wbox inspect` 的 `State.Paused` 写死 `false`。
结果是用户没有办法把暂停的容器和正常跑的区分开——而 `Paused` 这个字段比不存在
更糟：docker 用户会拿 `.State.Paused` 去写脚本，它却结构性地永远为假。

**状态从 `/proc` 实测，不记账**。记一个「我 pause 过」的标记文件更省事，但那份账
会过期：容器进程可能被别的途径 `SIGCONT` 起来，或者整个退掉又换了 pid。这里直接
看容器里的进程处于什么状态，`T` 就是被停住了——与 `liveness` 靠锁文件而不是靠 pid
判活是同一条思路：**能从系统真相直接读出来的，就不要另记一份账**。

判据是「有进程，且**每一个**都处于 `T`」：`pause` 是给整棵进程树发 SIGSTOP 的，
只有部分停住说明它并非处于 pause 状态（可能是 guest 内部自己停了某个进程）。

门禁两条一起才有意义：PZ.4 验暂停时 `ps` 与 `inspect` 都看得见，PZ.5 验 `unpause`
后**变得回去**——只会往一个方向变的「状态」等于没有状态。

#### F9.33 `inspect` 如实反映挂载与端口

**F9.33 `inspect` 如实反映挂载与端口** `[done]`（门禁 INS.1–INS.3）。

与 F9.32 同一类缺陷，同一个手法找到的：容器**明明挂着卷、发布着端口**
（`mount | grep /mnt/data` 数得到，端口也确实在转发），`wbox inspect` 却报
`"Mounts": []`，端口信息则根本不出现。给一个恒空的字段比不给更糟——docker 用户会
拿 `.Mounts` 和 `.HostConfig.PortBindings` 去写脚本。

修法是让状态记录里**真的存着这两样**：`ExecContext` 加 `volumes`/`ports`，在登记
容器时从 `RunSpec` 填入（形式与用户在命令行上写的一致，看到就知道对应哪个
`-v`/`-p`）。`inspect` 从这里生成 docker 形状的 `Mounts` 与 `PortBindings`。
`-p` 只支持 TCP（F9.2），所以键写死 `80/tcp` 是**如实**而非偷懒。

**新字段缺失必须按空处理，不能用 `?`**：状态目录里可能躺着上一版 wbox 写的
`meta.json`，用 `?` 会让 `exec_context` 整个解析失败，那些容器会从 `ps` 里
**整个消失**。单测直接构造旧格式记录钉住这条。

门禁先确认容器**真的**挂上了（`mount | grep -c`），再比对 `inspect` 的输出——
否则比的是两个都为空的东西，什么也证明不了；INS.3 反向验没挂卷时如实为空，
免得修完变成凭空造条目。

#### F9.34 状态口径收敛与网络模式三态

**F9.34 状态口径收敛与网络模式三态** `[done]`（门禁 INS.4–INS.5）。

两处，都是同一个手法扫出来的。

**一、`NetworkMode` 少了一态。** `--network container:X` 起的容器**确实共享了对端
的 netns**（实测两边 `/proc/self/ns/net` 是同一个 inode），`inspect` 却报
`"NetworkMode": "none"`。根因是记录里只有 `allow_network` 这个两态开关，而网络
模式其实是三态。`ExecContext` 补 `network_container` 后，三态各自如实报出。

**二、状态标签此前有三份。** `ps`、`inspect`、`compose ps` 各写了一份
`match liveness { … }`，于是 F9.32 给前两处补上 `paused` 之后，`compose ps`
仍把暂停的服务显示成 `running`。收敛成 `cli::status::label` 一处，三处共用；
以后再加状态（比如 `restarting`）也只改这一个地方。

**顺带修掉一个会永久误报的判据**（PZ.4 偶发红查出来的）：`is_paused` 原本要求
**整棵进程树每一个进程都处于 `T`**。看着更严格，实际是错的——`pause` 只停「发信号
那一刻已存在的进程」（F9.21 已写明这条代价），一个刚 fork 出来的子进程会逃掉；
它自己很快退出，但**父进程被停住就没人回收它**，于是留下一个僵尸。按「每一个都要
T」来判，那个僵尸会让容器**永远**显示成 running——而它其实是个已经死了的进程。
与 RMF.4 那次 `kill -0` 把僵尸认成活着是同一个坑。改成看**容器 init 是否为 `T`**：
它是 SIGSTOP 一定送达、SIGCONT 也一定送达的那个，语义上正是「这个容器停了没有」的
答案。改后连跑四轮 PZ 全绿。

#### F9.35 命名卷

**F9.35 命名卷** `[done]`（门禁 VOL.1–VOL.6）。docker/podman 的 named volume。

**它和绑定挂载解决的不是同一个问题。** `-v /宿主/某处:/data` 要求调用方自己想好
数据放哪，路径写错就挂错；命名卷把「放哪」交给 wbox：`-v mydata:/data` 里的
`mydata` 是名字，实际落在 `~/.wbox/volumes/mydata`。容器删了卷还在，换个容器挂
同一个名字接着用——这正是有状态服务绕不开的东西，也是此前 wbox 完全缺失的一个
docker 一等概念。

**rootless 下完全可做**：卷就是用户 `$HOME` 下的一个目录，挂载复用 F9.1 已有的
绑定挂载路径。不要驱动、不要常驻服务、不要 root，与 §2.2 的前提不冲突。

**名字与路径的分界必须简单可预测**（VOL.3）：与 docker 同规则——**源里不含 `/`
就是卷名**。任何「先当路径试试、不存在就当名字」的聪明做法，都会把一次手滑变成
一个新建的空卷；反过来，含 `/` 的源仍按宿主路径处理、不存在时照旧拒绝，
那条既有的安全行为一个字没改。

**隐式建卷要出声**（VOL.2）。docker 也隐式建，但沉默地建会让 `-v mydta:/data`
这种手滑变成「数据不见了」。打一行「已创建命名卷 X（路径）」，用户当场就能发现
名字打错了——既拿到了 docker 的便利，又没有它那个沉默的坑。

**删卷时挡住正在使用的**（VOL.4）。卷目录被抽掉，容器里那个挂载点就悬空了，
之后写进去的数据谁也找不回来。「谁在用」不另记引用计数，而是从容器记录现算
（`ExecContext.volumes`，F9.33 已经记了）——引用计数会在容器崩溃时过期，现算
永远是当前的。`-f` 可强删，并在错误里说清强删的后果。

#### F9.36 `--entrypoint` / `--env-file` / 四条 Dockerfile 指令

**F9.36 `--entrypoint` / `--env-file` / 四条 Dockerfile 指令** `[done]`（门禁 EP.1–EP.7）。

**`--entrypoint`**。`Some("")` 与 `None` 必须分开：docker 用 `--entrypoint ""` 来
彻底甩掉镜像的 entrypoint（`--entrypoint "" IMAGE sh` 是标准写法），用空串当
「没给」会让那个用法失效。覆盖之后**不再回落镜像 `Cmd`**——镜像的 Cmd 是给它自己
那个 entrypoint 当参数的，换了 entrypoint 还塞那串参数多半是错的（docker 同此语义）。
宿主程序模式没有镜像、也就没有可覆盖的东西，**明确拒绝**而不是静默忽略：
静默忽略会让用户以为自己换掉了要跑的程序，实际跑的还是原来那个（EP.3）。

**`--env-file`** 存在的理由是**别把密钥写进命令行**——`-e TOKEN=...` 会进 shell
历史，也会被同机别的用户从 `/proc` 看到。**不做变量展开、不去引号**（docker 也不做）：
做了的话文件里的 `PASS=a"b` 会被悄悄改写，而用户无从知道自己拿到的值和写下的不一样。
值里可以有 `=`（`URL=k=v` 合法），所以只切第一个。格式错要指出**第几行**——
几十行的文件不说行号无从查起（EP.5）。

**四条 Dockerfile 指令**：`LABEL`/`EXPOSE`/`USER` 写进镜像 config（形状与 OCI 一致：
`ExposedPorts` 的值是空对象，`EXPOSE 80` 补成 `80/tcp`）；**`ARG` 刻意不写进 config**
——构建参数常带凭证（token、密码），落进镜像等于随镜像一起发出去，EP.7 直接在
产出的 config 里搜构建参数来盯这条。

其中两条是**纯声明，必须说清它们不做什么**：`EXPOSE` 不会真的发布端口（发布要
`-p`）；`USER` 只是镜像声明的默认身份，运行时是否生效取决于 `--user` 与那一格
支不支持（F9.7）。把声明当成生效，是这两条指令最常见的误解。

**顺带消掉一处刚制造的重复**：`--entrypoint` 最初写成 `merged_command_with`，
与原 `merged_command` 构成「一个是另一个的特例」的两份东西。补的时候正是漏改了
其中一个调用点（`create` 改了、`run` 没改），实测才发现——于是合并成一个函数，
覆盖参数作为必填的 `Option`。

#### F9.37 容器内工作目录

**F9.37 容器内工作目录** `[done]`（仅 Linux 镜像模式，门禁 WD.1–WD.5）。

**这是一个静默丢弃的缺陷，不是缺功能**：镜像 config 里声明了 `WorkingDir: /app`，
容器照样起在 `/`；用户写了 `-w /other`，同样毫无反应。两者都**没有任何提示**，
而 `WORKDIR /app` 是 Dockerfile 里最常见的指令之一——相对路径全部指错地方，
症状离原因很远。

**根因是一个被重载的字段**：`RunSpec.workdir` 在镜像模式下是**镜像 rootfs 的宿主
路径**，在宿主程序模式下才是工作目录。同一个名字担了两件事，于是「容器内的 cwd」
这个概念根本无处表达，`-w` 解析出来后就没人接。补 `guest_workdir` 把两者分开，
并在两个字段的文档里互相点明——重载字段最容易在下一次扩展时再咬一口。

**目录不存在时逐级创建**（docker 同此行为，WD.3），但**建在哪是关键**（WD.4）：
必须换根之后再建。换根前 `/` 还是宿主视角，往 `rootfs/<w>` 里建会写进**共享的
镜像缓存**——那正是 F9.12 花力气避免的污染。换根后 `/` 已是 overlay 合并视图，
写入自然落在本容器的 upper 里。路径前缀在 fork 前算好：`pre_exec` 里不能分配内存。

**工作目录里的 `..` 直接拒绝**，不做消解：消解出来的路径与用户写的不是同一个，
出问题时对不上。

宿主程序模式不换根，`-w` 仍是宿主工作目录，语义一个字没改（WD.5 专门盯住这条，
免得改动把另一半带歪）。

#### F9.38 `ADD` 与 `ps --filter`

**F9.38 `ADD` 与 `ps --filter`** `[done]`（门禁 AF.1–AF.7）。

**`ADD` 与 `COPY` 只差一条**：源是本地 tar 时自动解开。识别**看内容不看扩展名**
（偏移 257 处的 `ustar` 魔数）——`ADD payload /x` 里那个没后缀的文件可能就是 tar，
而 `notes.tar` 也可能只是名字里带 tar 的文本。解包的安全约束与 `wbox import`
同一套（F9.25）：逐条挡绝对路径与 `..`，一律拼到目标之下——Dockerfile 是本仓输入
不假，但 `ADD` 的那个 tar 常常是第三方下载来的。

**远程 URL 明确不做**（AF.2），而且是**拒绝**不是静默当路径：构建期出网拿不到缓存
与校验，且常被网络策略挡住——而 wbox 的目标用户里正有「出网要审批」的那一类
（§3.1）。要取远程文件用 `RUN` + 自己信任的下载工具，校验与重试都在自己手里。

**`ADD` 必须和 `COPY` 一起进构建缓存键**（AF.4）。只哈希指令文本的话，源文件改了
键不变，后续步骤会命中旧快照，把**旧内容悄悄烤进镜像**——构建「成功」，内容是错的。
这条是加 `ADD` 时顺带查出来的：新指令加进枚举很容易，忘记同步 `step_key`
与 `mutating` 两处判定却不会有任何报错。

**`ps --filter`** 支持 `status=` 与 `name=`（子串，与 docker 一致），多个条件是**与**
关系。状态判定**复用 `status::label` 同一份口径**——两处各判一次的话，
`--filter status=paused` 会因为对「什么叫 paused」的理解不同而筛出错的东西。
**认不得的过滤键当场报错**（AF.7）：静默忽略会让 `--filter stauts=running` 这种手滑
变成「列出了全部」，而用户以为自己筛过了。

**多阶段构建（`COPY --from`）当时认得写法但明确拒绝**（AF.3）。当成普通 `COPY` 更糟——
找不到同名文件时报一个与真实原因无关的错，找得到则**悄悄打包了错的东西**。
它要求把每个 `FROM` 分成独立阶段各建一棵 rootfs，是 `run_build` 的结构性改造；
不在同一轮里既动那个函数又赶别的功能，列进了 §2.4.3 的下一步。

> **这一段已被 F9.39 取代**：多阶段构建随后实现了（门禁 MS.1–MS.5），
> `COPY --from=<阶段名>` 现在**正常工作**而不是拒绝。保留这段是因为 AF.3
> 这个门禁 ID 仍在脚本里，且"为什么当时选择拒绝而不是当普通 COPY"的论证
> 对下一次遇到同类取舍仍然有效。当前行为以 F9.39 为准。

#### F9.39 多阶段构建

**F9.39 多阶段构建** `[done]`（门禁 MS.1–MS.5）。`FROM <镜像> AS <名字>` +
`COPY --from=<名字>`。

**这一格的意义是「拿产物、不拿工具链」**：编译阶段装了编译器、拉了依赖，
最终镜像只要那个二进制。所以判据有两条而不是一条——产物**拿到了**（MS.1 前半），
且那个阶段**本身没进最终镜像**（MS.1 后半，直接在产物里检查中间文件不存在）。
只验前半的话，一个「把两个阶段合并成一棵 rootfs」的错误实现也能过。

**各阶段 config 独立**（MS.2）：A 阶段的 `ENV`/`CMD` 不该漏进 B 阶段，最终镜像
只带最后一个阶段的配置（docker 同此语义）。实现上是每遇到 `FROM` 就重置累加器。

**多阶段时禁用前缀缓存，并出声**（MS.5）。缓存键沿指令序列累加，跨阶段复用快照会
把 A 阶段的 rootfs 恢复到 B 阶段头上——那是**错的**缓存，而错的缓存比没有缓存糟
得多：构建「成功」，内容是别的阶段的。宁可不缓存，并打一行说明（不说的话用户会
困惑「为什么这个 Dockerfile 从来不命中」）。单阶段的缓存必须照常工作，MS.5 同时
盯住这一条，免得一刀切关掉。

**实现是「切换目标目录」而不是「重构成多次构建」**：每个 `FROM` 把产物目录切到
该阶段自己的位置（非最终阶段用临时目录，最终阶段用真正的输出目录），阶段名 →
rootfs 记进一张表供 `COPY --from` 取。这样指令执行的那一大段逻辑一个字没动——
它跑着两百多条门禁，能不动就不动。临时目录在构建结束时清掉（MS.3），
否则镜像缓存旁边会堆一堆半成品 rootfs。

**`--from` 的源必须是绝对路径**：它在那个阶段的 rootfs 内，相对路径没有可解释的
基准点。取源仍走 `resolve_rootfs_path`，复用同一份 `..` 逃逸校验。

### 4.9 跨宿主协作交接点

本节是 Windows 与 Linux agent 共享的工作面。队列按**最终必须在哪台宿主验收**
划分，不按谁提出、谁写代码或条目历史编号划分。谁的宿主谁验证，拿不到的机器
不硬猜。

无法在本机验证的东西**不写进产品代码**，改写成这里的一个条目：说清背景、
判据、以及"怎样算做完"。这不是甩锅，是本轮反复吃亏后的结论——两次把 CI
弄红都源于我在没有 Windows/wine 的机器上写 Windows 侧脚本，其中一次测的
根本不是我以为的东西（msys 把 guest 路径 `/busybox` 改写成宿主路径，
造出一个纯自伤的假失败）。

协作约定：

1. 每个 agent 开工前只从自己的宿主树领取任务；完成后在原条目更新状态和证据。
2. 任一 agent 都可以向另一棵树投递任务，但必须写明复现/背景、验收判据和完成标准。
3. 条目编号一旦被引用就保持稳定；`W5` 这类历史编号不代表当前归属，以所在树为准。
4. 能写代码但不能完成目标宿主验收时，状态最多是 `[active]`，并把剩余验收留在
   对应宿主树；不得仅凭另一宿主的单元测试标 `[done]`。
5. 两台机器都直接使用 `main`，领取和提交前先同步远端；冲突按双方任务意图合并，
   不覆盖另一 agent 的并行成果。

#### 4.9.1 [TODO-WINDOW]

```text
TODO-WINDOW
├── W1 Windows 侧 stop 的持续门禁                         [done]
├── W2 F8.4 exec 的 Windows 原生可对齐子集                [done]
├── W3 F9.4 Windows 文件系统写重定向取证                  [done] WP.27 + Rust i686 probe
├── W4 build 在 Windows 宿主的可行性                      [done]
├── W6 Q1 临时目录私有化（TEMP/TMP）                      [done] WP.25
├── W7 Q1 只读授权粒度（ACL 只读 ACE）                   [done] Win32 真机门禁
├── W8 Q1 capability 粒度（不止 INTERNET_CLIENT）        [active] 等外部 private peer
├── W9 create → rename → start 生命周期损坏               [done] WP.24
├── W10 资源限额的**行为**门禁（超限真的发生了吗）        [done] WP.26
├── W11 detached 启动 READY/ERROR 握手                     [done] WP.23A/WP.23B
├── W12 构建后清理 target 增量与测试垃圾                   [done] scripts/build.ps1
├── W13 Ubuntu 24.04 Windows 产品门禁                      [done] WU.1/WU.2
├── W14 跨宿主提交的 Windows test target 持续门禁           [done] 见下方 W14
├── W15 快速 lint、分层验证与后台只读观察工作流              [done] scripts/check.ps1
├── W16 Windows guest O_CREAT mode/umask 宿主解耦            [active] t_fd_open 0604→0644
├── W17 Linux signal 修复的 Windows 跨宿主验收               [planned] SIG_IGN/exec/setitimer
├── W18 guest known-failure 收紧到断言级                     [planned] 先拆 t_signalfd
└── R8 是否合并成单一 wbox.exe                            [待决] 见本节下方；不是 Rust-only 的阻塞项
```

`W16` 来自 Windows `guest-tests` 的基线外回归：
`t_fd_open/open/ocreat-mode` 以 `0604` 创建文件，`fstat` 却固定得到 `0644`。
根因不能归为 NTFS 没有 Unix mode 后直接豁免；纯 Rust guest 内核必须维护自己的
权限语义，新建 mode 应先应用 guest `umask`，随后由 `fstat`/`stat`/`statx`
一致返回，并且不能因宿主默认权限、rename 或 hardlink 改变。**禁止**把
`t_fd_open` 加回 `known-failures` 或放宽断言。完成判据是 Windows 真机
`t_fd_open` 86/0，且 Linux 结果不倒退。

`W17` 在 Linux agent 完成 signal disposition 与投递后接手 Windows 最终验收：
至少覆盖 `SIG_IGN`、`execve` 重置 caught disposition 并保留 ignored
disposition、`setitimer(which, NULL, old)` 解除 timer，以及既有 Ubuntu 24.04
`WU.1/WU.2` 产品门禁。另一宿主的单元测试不能替代这里的 Windows guest 证据。

`W18` 修复文件级基线会吞掉同一二进制内新回归的问题。当前 `t_signalfd`
已有 82 条断言通过、仅 handler 投递相关断言失败，继续豁免整个文件会让这
82 条重新变红时仍被放行。优先把 handler-delivery 拆成独立 guest 用例；
完成判据是已通过的 signalfd 行为处于基线之外，基线只包含尚未实现的能力。

`W13` 固定 Ubuntu 24.04 linux/amd64 manifest digest，由 Linux CI 生成最小
runtime fixture；`WU.1` 在 Windows 校验来源并授予 AppContainer 只读 ACL，
`WU.2` 在 Windows AppContainer 内经纯 Rust
`wbox-linux` 启动 glibc/Bash，核验 `os-release`、`uname`、APT 2.8.3、dpkg
amd64、getconf 64 bit、rc=37 透传及前台状态清理。接门禁时修复了 Windows
`fstat` 把所有文件合成为同一 `(st_dev, st_ino)`、导致 glibc 把 libc 误判成
已加载 libtinfo 的问题，并补齐 FXSAVE/FXRSTOR 与 MXCSR 指令族。

`W14` 来自 2026-07-30 对 Linux agent 四个提交的 Windows 复核。`FdTable::alloc`
改为返回 `Option<i32>`、`Fd` 状态字段重构、syscall helper 改名后，共享及
Windows 专属测试没有同步，导致 Windows test target 编译失败；HTTP 总期限测试
另触发 `clippy::unused_io_amount`，并在 Windows 上暴露原生 WSA 超时文案未归一化。
现已同步测试 API、把 DNS 与逐地址 connect 纳入同一请求期限、统一整体超时错误，
并让机会性真实 pull 测试使用 5 秒预算，不能继承产品的 30 分钟预算。
`wbox-tls` 的全量根证书自签验签仍全部保留，但 test profile 对该第一方密码学
crate 启用优化，避免 Windows debug 大整数运算让单测一项超过数分钟。Windows
workspace、Clippy、WP 全套及 WU.1/WU.2 均已复核通过。

`W15` 把 PRD → 实现 → owning gate 的日常反馈收敛成两条命令：
`scripts/check.ps1 -Quick` 先跑静态语法、rustfmt、host Clippy 与库测试，
`scripts/check.ps1` 再跑完整 workspace Rust tests；目标专属 G2/G3 仍由既有
产品脚本负责，不复制判据。后台 subagent 只能增量观察 HEAD、CI、失败摘要与磁盘
趋势，不争抢 Cargo 构建锁或改共享热文件；最终集成与串行门禁由前台 agent 负责。
CI 同一 workflow/ref 的旧运行会被新 push 取消，`build-wbox-linux` 也统一使用
locked 依赖和 Rust cache，减少过时构建与重复编译。后续复核又补齐：CI Rust
固定为 1.97.1；`preflight` 先跑静态语法、rustfmt 与 host all-targets check，
昂贵 job 只在它通过后启动；Quick 默认保留 incremental 热缓存，需要回收空间时
显式 `-CleanIncremental`，构建 wrapper 仍按 W12/L11 默认清理。本地精确 channel
pin 等离线工具链镜像可用后再加，避免只有 `stable` 别名的机器为同版本联网安装。

`W11` 已完成。detached 父进程不再把“supervisor 进程创建成功”当成容器启动成功：
Windows 后端在 `CreateProcessW -> AssignProcessToJobObject -> ResumeThread` 全部完成后，
Linux 后端在 `Command::spawn`（含 exec）成功后，才原子发布 READY。READY 前失败由
supervisor 发布 ERROR，父进程保留原始错误类别和退出码；父进程读完后再清理首次
`run -d` 的失败记录，或把 `start/restart` 的保存配置恢复成 created。READY 等待有界，
超时会终止 supervisor，不能出现调用方报失败、后台后来又自行跑起来的分叉。

极短的 `run -d --rm` 还有反向竞态：supervisor 已发布 READY 并正常退出，但紧接着
删除了 READY 所在的状态目录，等待中的父进程可能只看到 rc=0 的 supervisor 消失，
误报“发送 READY 前退出”。自动删除路径现在最多等待 5 秒让父进程消费 READY 后
再删目录；父进程异常退出也不会永久拖住 supervisor。`WP.7A` 连续跑 10 个短命
workload，同时断言全部启动成功且状态/日志最终归零。

`WP.23A` 用不存在的 Windows EXE 断言 `CreateProcessW` 原错与 rc=4；`WP.23B`
用本机拒绝连接的 registry 断言 pull 原错与 rc=5。两条都检查 `ps --all` 没有残留。

**Linux 侧补了一处**：`Command::spawn()` 在这里**不是**"exec 完就返回"。
PID namespace 要求 guest 是新 ns 的 PID 1，所以 `pre_exec` 里还会再 fork 一次
（见 `linux_ns.rs` 顶部"PID namespace 的双 fork"），而中间进程**永不 exec**——
它手里那份 `Command` 的 exec 状态管道写端就一直开着，`spawn()` 于是要等到整个
容器结束才返回。只用 `spawn() + wait()` 时这只是把等待挪了个地方，看不出来；
可 READY 一旦发布在 `spawn()` 之后，`wbox run -d -- sleep 9` 就要整整 9 秒才
返回，`--detach` 名存实亡（实测；门禁 P.9 判的正是"≤3s 返回"）。修法是让中间
进程 fork 完就 `close_range(3, ~0)` 把继承来的非 stdio fd 全丢掉——它只需
`waitpid` + `_exit`，一个 fd 都不要。exec 失败照旧立刻报原错，W11 的语义不变。

`W9` 同批关闭。状态目录名是 rename 后的事实源；`activate_created` 每次领取
`create.json`/`run-args.json` 时只在 `--` 之前把保存的 `--name` 归一成当前名称，
不会误改 workload 自己的同名参数。`WP.24` 在 Windows 真机执行
`create old -> rename old new -> start new`，并确认新名称获得非零 supervisor/guest
进程、旧名称不再出现。

`W10` 已由 `WP.26` 关闭。门禁先让 PowerShell 在无限额 Job 中保留 24 个 8 MiB
块，证明 workload 与宿主容量可完成该操作；随后只增加 `--memory 64` 重跑同一
循环，必须由 workload 自身捕获 `OutOfMemoryException`。这与 `job.rs` 对
`JOB_OBJECT_LIMIT_JOB_MEMORY` 的结构查询证据互补：前者证明公开 CLI 的行为，
后者钉住限额作用于完整 Job 而不是单进程。

##### W1 Windows 侧 `stop` 的持续门禁 `[Windows agent]` `[done]`

`WP.8-WP.12` 已加入 `test-windows-product.ps1` 并在 Windows 实机通过：
detached workload 用专属 PID 文件证明 supervisor、guest、child 三层在 stop
前全部存活；stop 后三个 PID 全部消失，记录转 exited，重复 stop 幂等，未知
名称失败，rm 清理记录。CI 30250676453 已通过。

门禁最初把 PowerShell `2>&1 | Out-String` 等待长命 supervisor 的现象误判为
测试编排限制，并绕成只等待短命父 wbox 进程。后续 `create/start` 门禁复现后
确认这是产品脚本兼容缺口：supervisor 继承了调用方管道句柄。F8.f 已在 spawn
边界收紧标准句柄继承，WP.22 改为直接重定向并等待 EOF，不再绕开问题。

##### W2 F8.4 `exec` 的 Windows 原生可对齐子集 `[Windows agent]` `[done]`

**结论：只能部分对齐，原生目标可实现，OCI/模拟器 目标不可可靠实现。**

Windows 原生 exec 派生运行中容器的同一 AppContainer SID，按记录重建
INTERNET_CLIENT capability，并把挂起创建的新进程加入同一命名 Job 后再恢复。
因此隔离身份、网络策略和 Job 资源限额对齐；工作目录从不含秘密的
`exec_context` 重建。wbox 原生模式本就不做文件系统虚拟化，所以文件系统仍是
同一组经 ACL 授权的宿主路径。

环境变量不写入状态文件，避免把 token、密码和调用方秘密落盘，因此当前 exec
只使用最小清洗环境，不继承原 `run -e`。Windows OCI/模拟器 还需要重建 rootfs、
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

##### W3 F9.4 Windows 文件系统写重定向的可行性取证 `[Windows agent]`

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

---

**结构性分析（Linux agent 读本仓库代码得出，不需要 Windows 机器）。**
它不替代实机取证，但把剩下要测的东西收窄了一大截：

*拒绝那一档已经成立，而且是免费的。* `acl.rs` 的文档说得很清楚：AppContainer
令牌 + Low 完整性级别下，子进程**默认读不到** `%USERPROFILE%\.wbox\...\rootfs`，
必须显式授 `ALL APPLICATION PACKAGES` 的读 ACE 才行。换句话说，
**默认姿态就是拒绝**——我们的 `grant_read_recursive` / `grant_modify_recursive_for_profile`
是在**打开**口子，不是在关。所以候选路径 2（ACL 拒绝）不必再取证：它已经是
现状，且不装驱动。

*重定向那一档，差的是"介入点"，不是 API。* 这一条是两个象限的结构差异：

- **Q2（Linux 镜像）有介入点**：guest 的每次文件操作都过模拟器的 VFS
  （`vfs.c` / `hostfs.c`），所以 Windows 侧的 `-v` 才可能靠 broker + VFS 数据面
  做出来——那正是 Windows agent 在推进的路。
- **Q1（原生 Windows 程序）没有介入点**：PE 程序发的是真的 NT 系统调用，
  wbox 的架构里没有任何东西在这条路径上。要重定向就得往目标进程里注入并挂钩
  API，那是与 minifilter 完全不同的一套代价（逐 API 覆盖、易被绕过、
  与 AppContainer 的注入限制冲突），**不是"换个 Win32 调用"就能做的**。

*因此结论的大致形状可以先写下来*：Q1 的写重定向在"不装驱动 + 不注入"的前提下
**做不到**；能兑现的是"拒绝 + 显式授权"，即当前状态。

**Windows 真机结论（2026-07-29）**：

1. UAC VirtualStore 在 AppContainer 下不生效。`dev/windows-virtualstore-probe.rs`
   由 `scripts/probe-windows-virtualstore.ps1` 编译为 i686；探针运行时确认
   `POINTER_WIDTH=32 MANIFEST_PRESENT=false`，再写 Program Files。原始裁决是
   `WRITE_ERROR kind=PermissionDenied os_error=Some(5)`，真实路径与
   `%LOCALAPPDATA%\VirtualStore\Program Files\...` 均不存在。该探针修正了旧方案
   使用带 manifest 的系统 `cmd.exe` 会产生假阴性的缺陷。
2. per-package 存储可用于普通非 UWP 程序。PowerShell 在容器内看到
   `LOCALAPPDATA=C:\Users\wjc2022\AppData\Local\Packages\w3b\AC`，写入和读回
   成功；宿主真实 `%LOCALAPPDATA%\wbox-probe.txt` 不存在，前台退出后 package
   随 profile 清理。WP.27 将这条转为持续门禁。

因此 Q1 的透明任意路径写重定向确定为**不做**；可承诺的是 Windows 自动提供的
package 私有标准目录和 wbox 的显式授权目录。前者是临时沙箱内容，不是 Sandboxie
式命名沙箱持久层，因为当前 profile 生命周期结束时会清理 package。

##### W4 `build` 在 Windows 宿主的可行性 `[Windows agent]` `[done]`

Windows 已能执行 F9.3 子集。`FROM` 先复制到 staging rootfs，`RUN` 复用
AppContainer + 模拟器运行路径并默认授予网络 capability；每一步使用临时容器记录，
满足 NativeBackend 的 Job/停止协议。构建成功后不直接 rename staging，因为其 DACL
含临时 profile SID 的修改 ACE；发布阶段重新创建目录、只复制文件内容与 symlink，
让最终镜像继承干净 DACL。

Windows symlink 复制复用模拟器的逃逸约束。Linux `/etc/...` 根路径按容器根解析；
已经重写到 staging 内的 `C:\...` 目标按 Windows 绝对路径解析，二者最终都必须
`strip_prefix(source_root)` 成功。

WP.18 在 Windows 真机从 fixture 构建 `COPY + RUN + CMD` 镜像，立即重建必须命中
`CACHED`；运行产物必须同时输出 COPY/RUN 标记，并断言基础镜像未被修改、staging
无残留。本机与 CI 30265370299 均已通过。

##### W6/W7/W8 Q1 的三条可做项 `[Windows agent]`

三条都来自 §2.4.4 的 Q1 路线图，最初由 Linux 侧 agent 提出。W6 现已完成
Windows 实测；W7/W8 仍只是基于 Windows 机制的可行性判断。接手的 Windows agent
**先验证再实现**；验证不成立就把条目改成「不做」并写明原因，那比留一个永远做不成
的待办诚实（这条纪律与 W3 一致）。

**W6 临时目录私有化** `[done：WP.25]`。
`--private-tmp` 在 Linux 把 `TMPDIR`/`TEMP`/`TMP` 指向 `<状态目录>/tmp`；
Windows 下 `TMPDIR` 保持该值，AppContainer 将 `TEMP`/`TMP` 改到 package 专属
`AC\Temp`。

**这一条原本整条挂在「待 Windows 验证」，其实不必**：它的机制是「建一个目录 +
注入三个环境变量」，**完全是平台中立的**，Q1 与 Q4 走的还是同一个宿主程序模式。
所以在 Linux 侧就实现并取证了（门禁 PT.1–PT.5）。2026-07-28 Windows 实机取证
确认：AppContainer 内 `TMPDIR` 保留为 `<状态目录>/tmp`，但 `TEMP`/`TMP` 会被
Windows 改写为该 AppContainer package 的 `AC\Temp`。两者都是容器私有位置，
但“三个变量都指向同一路径”的描述不成立。

已在 Linux 侧钉住的：缺口真实存在（PT.1 先证明不加选项时写 `/tmp` 确实落在宿主）、
私有目录可写且不泄漏到宿主（PT.2）、三个变量都设（PT.3）、
**用户显式的 `-e TMPDIR=` 覆盖注入值**（PT.4——静默改写用户的显式意图是最难查的
那类行为）、镜像模式明确拒绝（PT.5——换根后 `/tmp` 本就在容器可写层里）。

**边界要写明，别让它被读成「临时文件一定被隔离」**：这只覆盖**遵守
`TMPDIR`/`TEMP`/`TMP` 约定**的程序。硬编码路径的程序挡不住——那需要写重定向，
而那正是 §2.4.2 说清的、用户态做不到的那一半。

Windows 真机门禁 `WP.25` 已用 PowerShell 分别在状态目录 `TMPDIR` 与 package
`TEMP`/`TMP` 创建并读回文件，证明两处都可写；另一次运行证明
`-e TMPDIR=...` 的用户覆盖在 AppContainer 下仍成立。状态目录在前台进程退出后
随运行记录一起清理。为了让 AppContainer 真能写状态目录，登记后创建私有 `tmp`，
再向该容器的确定性 profile SID 授予文件级 modify 权限；权限不含
`WRITE_DAC`/`WRITE_OWNER`，没有用 `GENERIC_ALL` 扩大边界。

**W7 只读授权粒度** `[done：Win32 真机门禁]`。共享镜像缓存已经通过
`grant_read_recursive` 获得 RX ACE，私有 rootfs/tmp 则只向容器 profile SID
授予不含 `WRITE_DAC`/`WRITE_OWNER` 的 modify 权限。真机测试
`read_only_acl_allows_reads_and_denies_writes` 复制测试进程并以 AppContainer +
Job 启动：子进程能读取 canary，但覆盖已有文件和创建新文件都得到
`PermissionDenied`。

该结论只证明**授权粒度**，不提供路径映射。Q1 原生 Windows 程序没有 mount/VFS
介入点，因此 `-v host:guest:ro` 仍明确拒绝；未来 Q2 guest VFS volume 可以把
只读 ACE 作为宿主侧最小授权，但还必须在每个 guest 写入口执行 `EROFS` 语义。

**W8 capability 粒度** `[active：等待外部 private peer]`。现在公开 CLI 只有
`INTERNET_CLIENT` 对应的 `--allow-network`。代码已能构造并核对
`INTERNET_CLIENT_SERVER`（`S-1-15-3-2`）和
`PRIVATE_NETWORK_CLIENT_SERVER`（`S-1-15-3-3`），但 SID 正确不等于流量生效。

Windows 真机已经排除两个错误判据：

1. `TcpListener::bind(0.0.0.0:0)` 在空 capability、仅客户端、Internet server、
   private server 四组下**全部成功**；过滤不发生在 bind 这一步。
2. 宿主适配器标为 Private 时，用该适配器地址连接同机 listener，在空 capability
   和 `PRIVATE_NETWORK_CLIENT_SERVER` 下都超时；同机路径另受 AppContainer
   loopback isolation 控制，不能替代第二台设备。

外部门禁 `private_network_capability_controls_external_endpoint` 已编码：用另一台
Windows-Private 网络中的机器提供 HTTP endpoint，先证明宿主可达，再要求空
capability 失败而 private capability 成功。运行方式：

```powershell
$env:WBOX_TEST_PRIVATE_ENDPOINT = "http://PRIVATE-PEER:PORT/"
cargo test private_network_capability_controls_external_endpoint -- --ignored --nocapture
```

在这条流量证据通过前不新增 `--allow-private-network` /
`--allow-network-server`，避免把无法验收的 SID 开关当成功能。若需要验证入站
server capability，还必须由外部 peer 主动连接 Windows AppContainer listener；
同机连接同样不能裁决。

##### R8 是否把模拟器合并进单一 `wbox.exe` `[Windows agent]` `[待决]`

**背景**：§2.2.1 的 Rust-only 约束已兑现（`vendor/blink` 删除，引擎是
`crates/wbox-linux`）。但发布物仍是**两个** exe：`wbox.exe` + `wbox-linux.exe`。
引擎同时提供 lib 与 bin 两种形态，`src/runtime` 是 lib 的进程内入口，
`EmuBackend` 走的是 bin 形态。

**待决的不是"能不能"，而是"合并后隔离怎么算"**：

当前 Windows 上的隔离是**双层**——`EmuBackend` 让 `wbox-linux.exe` 作为独立
进程跑在 AppContainer + Job 里（§2.4 Q2 的"双层隔离"就是指这个）。如果改成
`wbox.exe` 进程内执行 guest，那么：

- 承载 guest 的进程同时也是 CLI 与 supervisor 进程。要么整个 `wbox.exe` 自己
  降权进 AppContainer（那 supervisor 就失去了它需要的权限），要么 guest 不再被
  AppContainer 包住（**隔离降级，不可接受**）。
- 可行方向是 `wbox.exe` 以"再执行一次自己 + 隐藏子命令"的形式重入
  （类似 `wbox --internal-guest`），发布物仍是单文件，隔离层不变。
  这条路要先确认 AppContainer 内重入自身 exe 的路径可达性与 ACL。

**怎样算做完**：单文件发布物 + `WP.3` 在 AppContainer 内跑通 guest + 明确记录
guest 与 supervisor 的权限边界。做不到就如实保留两个 exe——两个 exe 都是纯
Rust，本身不违反 §2.2.1，只是分发上多一个文件。

#### 4.9.2 [TODO-LINUX]

```text
TODO-LINUX
├── L1 F8.4 exec 的 Linux 侧实现                          [done]
├── L2 Wine 象限的 wineprefix 隔离                        [done]
├── L3 `wbox push` 镜像推送                               [done] F9.13
├── L4 compose 子集                                       [done] F9.14
├── L5 镜像分层存储                                       [done] F9.16-F9.18
├── L6 pod 抽象是否值得做                                 [done] 结论是不做
├── W5 Q2 端口映射 `-p` 可行性取证（历史编号）            [done] 语义不适用
├── L7 `load` / `import` 解包的符号链接边界               [done] 见下方 L7
├── L8 `cp` 穿过 upper/rootfs 中间符号链接                [done] 门禁 CP.7/CP.8/AF.8；见下方 L8
├── L9 Linux native / Wine 共用后端验收当前失败            [done] 见下方 L9
├── L10 TLS 要不要也换成第一方实现                        [done] 做了，见下方
├── L11 构建后清理 target 增量与测试垃圾                   [done] scripts/build.sh
├── L12 guest VFS 的宿主符号链接逃逸（Critical）           [done] 见下方 L12
├── L13 自研 TLS 的 Docker Hub pull 有界超时与成功门禁      [done] 每次请求有整体预算；见下方 L13
├── L14 file description 级共享状态                       [done] F 组基线整组清空；见下方 L14
├── L15 socket 族与 epoll                                 [done] 跨宿主复核的四条 INET 缺口已补；见下方 L15
├── L16 eventfd                                           [done] t_eventfd 80/0 已移出基线
├── L17 timerfd                                           [done] t_timerfd 67/0 已移出基线（惰性到期，无后台线程）
├── L18 mount(2) 与 MS_RDONLY                             [done] t_mount_ro 13/0，E 组整组清空
├── L19 signalfd 与挂起信号集合                           [partial] t_signalfd 82/3；handler 投递未做
└── L20 MAP_SHARED 文件映射写回                           [done] t_mmap 140/0，G 组整组清空
```

`L13` 由 Windows W13 接门禁时发现：公开 CLI 对固定
`ubuntu@sha256:52df9b1e...cf3faf` 执行 `image pull`，超过 10 分钟仍无输出且
进程不退出；直接 `-V` 重跑也超过 3 分钟。

**根因是"每次 I/O 都有超时"并不等于"整体有上界"。** `wbox-http` 原先只在
socket 上设 `set_read_timeout(300s)`，那只挡得住"对端彻底不说话"；对端每隔
299 秒吐一个字节，每次 read 都在超时之内，**整体就能拖到无限久**。同一形状的
无界等待还有两处：`to_socket_addrs` 走系统 resolver、本身没有超时（DNS 卡住
时连接函数永远不返回）；以及 `wbox-tls` 握手里"收到 ChangeCipherSpec 就
continue"的循环——对端一直发 CCS 就一直转。

**修法是把期限下沉到 TCP 这一层**（`transport::DeadlineStream`）：每次 I/O
之前先看总预算，把 socket 超时压到 `min(单次超时, 剩余预算)`，预算用完直接
报 `TimedOut`。放在这一层而不是 HTTP 层，是因为 TLS 握手也跑在这条流上，
那几个循环在 `wbox-tls` 内部、HTTP 层够不着——把上界放在字节管道上，谁在
上面跑都被兜住。DNS 另起线程 + `recv_timeout`。

预算按**一次 `request()` 调用**算并由重定向共享（否则 10 跳重定向就把上界
放大 10 倍）。registry 客户端取 `io_timeout=60s` / `total_timeout=1800s`：
每个 blob 是一次独立请求，按最慢 1 MB/s 算 30 分钟够拉 1.8 GB，而真卡住时
半小时必然报错返回。需要更长可用 `WBOX_HTTP_TOTAL_TIMEOUT`（秒）。

**判据非空已验证**：`slow_drip_peer_still_hits_the_total_budget` 用一个每
50 ms 吐一个字节、永不结束的假服务器——单次超时永远触发不了，只有总预算能
收场；把 `budget()` 短路成"永远返回单次超时"后，它当场在"读了 20 秒还在
继续"上变红。反向判据 `generous_budget_does_not_cut_a_healthy_read` 挡住
"永远立刻超时"那种假实现。真实 `wbox pull busybox:latest`（走自研 TLS 到
Docker Hub）已实机跑通。

##### L14 file description 级共享状态 `[Linux agent]` `[done]`

**已修**，`tests/known-failures.txt` 的 **F 组整组清空**（`t_fd_open` 86/0、
`t_fd_rw` 126/0），guest 套件 7 通过 → **9 通过 / 12 失败**。

**根因是把"文件描述符"和"打开文件描述"当成了一个东西。** POSIX 里
`O_APPEND`／`O_NONBLOCK` 这类状态标志属于**打开文件描述**（open file
description），只有 `FD_CLOEXEC` 属于描述符本身。原实现给每个 `Fd` 存了一份
`flags: i32`，于是 `dup` 之后两个别名的标志各走各的——在 `d2` 上
`F_SETFL O_APPEND`，`fd` 上 `F_GETFL` 看不到，写入也不追加。

改成 `Rc<Cell<i32>>`，`dup`/`dup2`/`F_DUPFD`/`fork` 共享同一份
（`Fd::alias`）。偏移本来就是共享的——`File::try_clone` 走宿主 `dup`。

顺带补齐同一簇的缺口，每一条都由用例逐条钉住：

- `O_APPEND` 的**写语义**：写前 `seek(End(0))`。此前只存了标志、从不生效。
- `ioctl(FIONBIO)`：它就是 `F_SETFL O_NONBLOCK` 的另一种写法，作用在描述上；
  原先一律报 `ENOTTY`。
- **管道容量**（`F_GET/SETPIPE_SZ`，默认 64 KiB，下界一页）与非阻塞写边界：
  写满报 `EAGAIN`、部分写只写得下的那些。容量**只对非阻塞写强制**——单线程
  快照式 fork 下 `a | b` 的 a 必须先跑完，真按容量阻塞会当场死锁；这条偏差
  记在 `PipeInner::capacity` 的文档里，不假装是完整语义。
- 管道的 `POLLHUP`／`POLLERR`／`EPIPE`：新增读端计数（`PipeReader` RAII，
  与写端对称）。另外 `POLLHUP`/`POLLERR`/`POLLNVAL` **不受 `events` 掩码
  约束**，原先整条按 `& events` 过滤，于是"写端已关"对只订阅 `POLLIN` 的
  调用方永远不可见。
- 管道 `fsync`/`fdatasync` 报 `EINVAL`（Linux 如此），此前一律报成功。

还有三个**同形态但不同入口**的分发错误，是顺着这两个用例挖出来的：

- `faccessat` 与 `linkat` 的 `dirfd` 在分发表里被整个丢掉，直接接到只认 cwd
  的 `sys_access`／`sys_link` 上。表现是 `openat(dirfd,…)` 读得到的文件，
  `faccessat(同一个 dirfd,…)` 却 `ENOENT`。顺手补了 `faccessat2`。
- 最小可用 fd 的下界写死成 3。Linux 给的是**最小空号**：`close(0); pipe(p);`
  之后 `p[0]` 必须是 0——那正是把 stdin 重定向成管道读端的惯用法。

##### L15 socket 族与 epoll `[Linux agent]` `[done]`

socket 族从**整族 `ENOSYS`** 做到：`t_negative` **40/0 已移出基线**，
`t_net_epoll` **111 通过 / 1 失败**，`t_net_sockopt` **145 通过 / 1 失败**。
后两条在**当前 guest 套件覆盖范围内**各自只剩 D 组那条快照 fork 语义差异。
2026-07-30 的跨宿主静态复核另外指出四条**没有被现有 guest 断言覆盖**的产品
语义缺口——每一条都属实，现已全部补上（复核提出的原文保留在下面，便于对照）：

- `Inet::Listener` 无条件报告 `EPOLLIN`，即使没有待 accept 连接；
  `Inet::Stream` 也无条件报告 `EPOLLIN|EPOLLOUT`，没有查询宿主 socket 的真实
  readiness。调用方可能被告知“可读”后在阻塞 read/accept 中挂住。
- `AF_INET/AF_INET6 + SOCK_DGRAM` 虽能在 `bind` 时创建 `UdpSocket`，但
  `read`/`write`/`sendto`/`recvfrom` 仍只走 `TcpStream` helper，目标地址参数也
  被 `sendto` 丢弃，UDP 实际不可用。
- INET socket 的连接状态存在 `Socket::inet`，但 `shutdown` 仍检查 AF_UNIX 的
  `Socket::state`，已连接 TCP 会被错误报成 `ENOTCONN`。
- INET `recvfrom` 在请求来源地址时调用 `write_sockaddr_un`，会回写 `AF_UNIX`
  而不是真实 IPv4/IPv6 peer；未绑定的 AF_INET6 `getsockname` 也合成 IPv4
  unspecified 地址。

**四条的修法**：

1. **就绪判定真去问宿主 socket**。监听者：做一次非阻塞 `accept`，取到的连接
   先存进 `Inet::ListenerPending`，`accept` 系统调用优先从那里拿——这样
   "报了可读"和"accept 不会阻塞"是同一件事，而不是两件。已连接流：用
   `peek` 探一个字节（不消费数据），`Ok(0)` 即对端已关，报 `EPOLLIN|EPOLLHUP`。
   无条件报就绪的老写法会让调用方在随后的阻塞 read/accept 里挂住，
   那正好抵消了 epoll 存在的意义。
2. **UDP 打通**：`sendto` 带目标地址时走 `UdpSocket::send_to`（面向连接的
   socket 上给地址报 `EISCONN`），`recvfrom` 走 `recv_from` 并回真实 peer，
   `read`/`write` 走 `recv`/未连接则 `EDESTADDRREQ`。
3. **`shutdown` 查对地方**：INET 的连接状态在 `Socket::inet` 里，老代码只看
   AF_UNIX 的 `Socket::state`，于是已连接的 TCP 一律被报成 `ENOTCONN`。
4. **地址族跟着 socket 走**：INET 的 `recvfrom`/`getsockname` 回 `sockaddr_in`
   /`sockaddr_in6`；未绑定的 AF_INET6 回 `[::]:0` 而不是 IPv4 通配地址——
   调用方按 `ss_family` 分支，回错族就会走错分支。

**顺带修一处自己引入的回归**：timerfd 那次把 `poll` 的等待改成"边等边复查"，
但唤醒条件漏了 `events` 掩码。于是只订阅 `POLLIN` 的 socketpair 读端会因为
"可写"立刻醒来，`poll(fd, POLLIN, 200)` 几毫秒就返回 0——而 guest 正拿它当
精确睡眠。`t_net_sockopt` 从 145 掉到 144，被这次复核连带发现。
- 阻塞式 INET `connect` 直接调用无期限的 `TcpStream::connect`；需要由 guest
  可中断/有界等待或明确记录其阻塞契约。

guest 套件 9 通过 → **10 通过 / 11 失败**，说明既有基线确实收紧，但在上述
反例补成自动测试并通过前不得把 L15 标成 `[done]`。

**AF_UNIX 由引擎自己实现，不转给宿主。** 两条理由，第二条更根本：

1. 宿主可能是 Windows，那里没有 `socketpair` 的等价物（Win10 1803 起的
   `AF_UNIX` 只支持路径式）。而 `socketpair` 恰恰是 guest 侧最常用的一种。
2. **语义要在两个宿主上一致**（F5）。做成引擎内的对象，两边就是同一份代码、
   同一套行为，不必去追平两个操作系统的差异。

代价如实记：这样的 AF_UNIX **只在同一个 wbox-linux 进程内连得通**，guest 连
不上宿主上别的程序的 unix socket。快照式 fork 的父子在同一个宿主进程里，
所以 fork 之后照样通。

覆盖面：`socket`/`socketpair`/`bind`/`listen`/`connect`/`accept4`/
`getsockname`/`getpeername`/`shutdown`/`get,setsockopt`/`sendto`/`recvfrom`，
流式与数据报两种（数据报保留消息边界），`SOCK_NONBLOCK`/`SOCK_CLOEXEC`，
`FIONREAD`，以及 socket 的 `lseek`→`ESPIPE`、`fsync`→`EINVAL`、
`fstat`→`S_ISSOCK`。

epoll：`epoll_create1`/`epoll_ctl`/`epoll_wait`，LT／ET／ONESHOT／MOD／DEL、
`RDHUP`／`HUP`／`ERR`、多 fd、以及 dup 别名的生命周期。三处值得记：

- **关注项指向"打开文件描述"而不是 fd 号，且必须用 `Weak`。** 持强引用会让
  被监视对象永远不死、永远"就绪"，于是前一组用例关掉的 socket 会在后一组的
  `epoll_wait` 里继续冒事件（实测就是这么撞上的）。`Weak` upgrade 失败 =
  "该描述已经没人引用"，正是内核那条自动摘除规则。`EPOLL_CTL_ADD` 判 `EEXIST`
  同理要比对象而不是 fd 号——号会被回收再分配。
- **边沿触发报的是"又有新东西了"这个瞬间**，不是"现在有东西"。单线程模拟器
  没有内核那样的写入钩子，只能在 `epoll_wait` 时回看，所以给管道与 socket 的
  缓冲各加了一个"写入代次"计数：代次变了就是一次新边沿。少了它，"读空 → 再
  写入"这条最常见的 ET 序列会被当成同一次就绪而漏报。
- **`poll`／`epoll_wait` 的就绪判定走同一个函数**。两处分叉的表现是同一个 fd
  在 `poll` 里可读、在 `epoll` 里不可读，那种不一致极难从现象追回根因。
  超时也真的睡满：单线程下没人能在这期间改变状态，但 guest 会拿
  `poll(NULL, 0, ms)` 当精确睡眠，立刻返回 0 是在撒谎。

**AF_INET/AF_INET6 走宿主套接字**（`std::net`），与 AF_UNIX 的取舍正好相反：
那边要的是"两个宿主行为一致"，这边要的是"真能连上外面"，而后者宿主已经给了
跨平台实现，自己再实现一遍 TCP 既不可能也没意义。三处值得记：

- **`bind` 时就建 `TcpListener`**（它同时完成 bind+listen）。因为 `getsockname`
  必须在 `bind` 之后立刻能回真实端口——绑 0 号端口让内核选一个再问回来，是
  "起一个临时监听者"最标准的写法。代价：给**客户端** socket 绑本地地址
  （少见用法）会被当成监听者。
- **非阻塞 `connect` 用后台线程**。`std` 没有非阻塞 connect，而 `EINPROGRESS`
  是这条路径上唯一正确的答案（libc 与几乎所有网络库靠它决定要不要去 poll
  可写）。与其为它写两套平台原生代码，不如把阻塞 connect 丢进一个线程：
  调用方立刻拿到 `EINPROGRESS`，之后 `poll(POLLOUT)`／`SO_ERROR` 取结果。
  `SO_ERROR` **取一次就清**，这是 Linux 语义——不清的话调用方会反复看到同一个
  旧错误。
- **每次读写前都按 guest 的 `O_NONBLOCK` 设一遍宿主 socket 的模式**，而不是
  建连接时设一次：那一位属于打开文件描述，guest 随时能用 `fcntl`/`ioctl`
  翻转它，而那两条路径改的是引擎自己的状态位，宿主 socket 并不知道。

**顺带把 `t_negative` 收干净了**：socket 的 errno 面补齐后，剩下的全是 rlimit。

- `setrlimit`/`prlimit64` 的**参数校验顺序**：先读 `new`（读不动就 `EFAULT`，
  且 `old` 一个字节都不写），再校验资源号，再校验 `cur <= max`，应用之后**才**
  写 `old`——所以"`old` 指针坏掉"返回 `EFAULT` 但**新限额已经生效**。这不是
  随手定的顺序，用例两头都钉死了。
- **`RLIMIT_NOFILE` 软上限真的卡住 fd 分配**。判断放在 `FdTable::alloc_min`
  里而不是各个调用方：分配点散着好几处，让每处自己先查一遍，漏掉的那处就是
  "限额说了不算"。
- **`pipe`/`socketpair` 要原子**：半途 `EMFILE` 却已占掉一个 fd 是最难查的那类
  泄漏——调用方看到失败、以为什么都没发生，fd 却少了一个。失败时输出缓冲也
  一个字节都不写。

##### L1 F8.4 `exec` 的 Linux 侧实现 `[Linux agent]` `[done]`

**已实现**（`wbox exec <NAME> -- <CMD>`），门禁 P.19–P.22。两个坑都是实测才
暴露的，写下来省得再踩：

**坑一：取不到容器内 pid。** 自然想法是用 `cmd.spawn()` 的返回值，但当时
**`cmd.spawn()` 在容器退出之前根本不返回**——PID namespace 的双 fork 里，中间
进程负责转发退出码、**永不 exec**，而 Rust 的 `Command::spawn()` 要等 CLOEXEC
错误管道读到 EOF 才返回，写端正握在它手里。这个坑很会骗人：短命容器上一切
"看起来正常"，因为你总是在它结束之后才去看文件。

（该阻塞本身后来在 W11 那条里修掉了：中间进程 fork 完就把继承来的非 stdio
fd 全关掉，`spawn()` 于是在孙进程 exec 时就返回。这里的"从宿主侧观察"做法
仍然成立且更稳，没有跟着改回去。）

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

##### L2 Wine 象限的 `wineprefix` 隔离 `[Linux agent]` `[done]`

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

##### L3 `wbox push` 镜像推送 `[Linux agent]` `[done]`

**已实现，见 F9.13**（门禁 PSH.1–PSH.5）。下面保留当初的设计判断，因为其中
"缓存里没有原始层 blob 所以只能 flatten"这条结论仍然约束着后续的分层存储那一格。

**先想清楚推什么**。缓存布局是**解包后的 rootfs**（`rootfs/` + 三个 json），
pull 时层 tar 已解开丢弃，没有原始 blob 可以原样回推。可行路线是 **flatten**：
把 rootfs 打成单层 tar.gz（`tar` crate 已在依赖树里，pull 解包用的就是它），
算 digest，生成单层 manifest + config 再推。这与 `docker commit` 后 push 的
语义一致，须在文档里说清"推出去的是平铺单层，不保留原始分层"。

协议是 OCI Distribution 的上传三步：`POST /v2/<repo>/blobs/uploads/` 拿
Location → `PUT <location>&digest=...` 单体上传（层 + config 各一次，先 HEAD
判存在可跳过）→ `PUT /v2/<repo>/manifests/<tag>`。`RegistryClient` 目前只有
GET/匿名 token；push 需要 POST/PUT + Basic 凭证换 token（凭证已有 F7 的存储与
脱敏约束，**只发给获准的认证端点**）。

**验证是难点**：不能拿真 registry 当门禁。建议门禁里用 python3 起一个最小
registry stub（收 POST/PUT、存内存、response 202/201），断言收到的 manifest
与 blob digest 对得上；再用 `wbox pull` 从 stub 拉回来跑通，形成闭环。

##### L4 compose 子集 `[任一 agent]` `[done]`

**已实现，见 F9.14**（门禁 CMP.1–CMP.7）。与下面当初的设想有一处不同：
最终**没有引 `serde_yaml`**（已归档停维护），改为手写有界子集解析器。

范围要先钉死，否则会滑向"重新实现 docker-compose"。建议第一刀只做：
`services.<name>.{image, command, volumes, ports, environment, depends_on,
restart, healthcheck}`，`up -d`/`down`/`ps` 三个动词，`depends_on` 只做启动
顺序不做 condition。YAML 解析引 `serde_yaml`（新依赖，需过 §2.2 的"纯 Rust、
无 C 编译"门槛——它满足）。网络语义：同一 compose 文件里的服务默认
`--network container:<第一个服务>` 共享 netns（F9.11 已就绪），经 localhost
互访，这与 docker 的 bridge+DNS 不同但对 sidecar 场景等价，文档要直说。

##### L5 镜像分层存储 `[Linux agent]` `[done]`

**两半都已完成。** 存储与 push 那半见 F9.16（门禁 PSH.6–PSH.7）：pull 保留原始
压缩层，多层镜像能原样回推且 manifest digest 不变——这正是下面写死的第一条判据。
`FROM` 复用基础层那半拆成 L5b，也已完成（F9.18，门禁 OVB.1–OVB.4）。

**先分清它和 F9.12 不是一回事。** F9.12 解决的是**运行期**写入污染共享缓存；
这一条要的是**存储期**保留镜像的层结构，两者的产物、生命周期、失效条件都不同。

要做的核心改动是缓存布局：现在只存解包后的 `rootfs/`，pull 时层 tar 解开就丢。
分层存储要求额外保存**原始压缩层 blob**（外加解包结果，或改为按需组装）。
牵动四条路径，每条都要重新想清楚：

- `pull`：多存一份 blob，磁盘占用大致翻倍——要不要做去重/GC 得先定。
- `build`：`FROM` 现在整份复制 rootfs；有了层就该改成引用基础层 + 新增层。
- `overlay`（F9.12）：lowerdir 可以从单一 rootfs 变成多层叠加，语义更接近 docker。
- `push`（F9.13）：现在只能 flatten 成单层，有了原始层才能原样回推、与上游共享层。

**判据**：pull 一个多层镜像后能原样 push 回去且 manifest digest 不变
（`[done]`，PSH.7）；`FROM` 复用基础层时磁盘占用明显低于整份复制（`[done]`，见
L5b 与 OVB.1–OVB.4）。

##### L5b `FROM` 复用基础层 `[Linux agent]` `[done]`

**两条判据都已达成**：网络侧 F9.17（push 时基础层被 `HEAD` 跳过，PSH.8b）、
磁盘侧 F9.18（硬链接共享，实测 1432KB vs 两份之和 2788KB，OVB.3）。

下面保留取证过程，因为它解释了**为什么最终是硬链接 + overlay 而不是别的**：

- **reflink 走不通**：`cp --reflink=always` 在 ext4 上直接
  `Operation not supported`（本机实测，`df -T` 为 ext4）。只有 btrfs/XFS 有。
  可以实现并在支持的文件系统上生效，但**在 ext4 机器上既无收益也无法验证**，
  按项目规矩（无法在本机验证的不写进产品代码）没有落地。
- **纯 hardlink 不安全**：`RUN` 的写入会就地修改共享 inode，直接**写坏基础镜像
  缓存**——这是最不该引入的一类缺陷（破坏的是别的镜像，且很晚才会被发现）。
- **最终采用**：build 的 `RUN` 走 overlay（lower=hardlink 出来的 staging，
  upper=每步一个），步骤结束后把 upper 以"先 unlink 再落盘"的方式合并回 staging，
  只对改动过的文件断开硬链接。overlay 不可用时退回整份复制。

  这条路的两处形态已经实测清楚，接手时不必再试：
  - 删除在 upper 里表现为**字符设备 0:0**（`c--------- 0, 0`），宿主侧
    `stat` 就能认出来；与 tar 层的 `.wh.` 前缀**不是**一套，合并代码要自己判。
  - 删除**目录**同样得到字符设备 0:0，但前提是挂载时带了 `userxattr`；
    不带的话直接 `EIO`（这个坑已经在 F9.12 修掉，见上）。

**剩余判据**：同一基础镜像上构建两个镜像时，磁盘占用明显低于两份整份复制。

##### L6 pod 抽象是否值得做 `[任一 agent]` `[done：结论是不做]`

**评估结论：不做，并已把促成这个结论的能力补齐。**

podman 的 pod 是共享 network/IPC/UTS 的一等抽象。评估先问"去掉网络之后还剩
什么"，答案是 IPC 与 UTS——而查代码发现 wbox **根本没隔离这两个**（容器直接
用宿主的）。于是先修缺口：F9.15 把 IPC/UTS 纳入默认隔离，并按 F9.11 同一套
机制提供 `--ipc`/`--uts container:<NAME>`。

补齐之后，pod 的三样共享（net/IPC/UTS）都能**单独且组合取得**，成组管理由
F9.14 的 compose 提供。再引入一个 pod 对象，得到的只是换个说法：多一个要维护
的生命周期实体（创建/删除/列举/与容器的从属关系），换不来任何现在做不到的事。

故 §2.4 Q3 的 pod 一行记为**不做**，理由指向这里。若将来出现"必须以 pod 为
单位调度/迁移"的需求，再回来重估——那时驱动它的是调度语义，不是 namespace
共享。

##### L7 归档解包的符号链接越界 `[TODO-LINUX]` `[done]`

**已修复。** 由 Windows 侧 agent 的代码审查指出：`load` / `import` /
Dockerfile 的 `ADD` 只做了**词法**路径检查（有没有 `..`、是不是绝对路径），
然后把拼好的目标交给不带根约束的解包函数。

**攻击形态**：归档先放一个 `evil -> /tmp` 的符号链接，再放一个名为
`evil/pwned` 的普通文件。`evil/pwned` 词法上完全合法，词法检查放行；
写下去时内核跟着符号链接走，文件落到 `/tmp/pwned`——**已经在根之外了**。

**修法**：`wbox_codec::tar` 新增 `Entry::unpack_in(root, rel)`，逐段检查
路径——任何一段如果已存在且**是符号链接**就拒绝，缺失的段按目录创建。
三个调用点（`oci::archive`、`cli::export`、`build`）全部切过去。
`oci::image` 的层解包**保持用 `unpack`**：那里有一整套 whiteout / 越权路径 /
符号链接规则，比这一层严格，两套叠加只会互相干扰。

**顺带修掉的两条**（同一次审查）：

- 硬链接目标按**归档根**解析，不是按进程 cwd（`fs::hard_link` 的默认行为
  会链到一个完全无关的文件上）；
- tar **绝不按头部声明的长度预分配**内存。一个 512 字节的归档可以声称里面
  有个 4 GiB 的文件，照着分配就是一次拒绝服务。改成让缓冲随实际到达的数据
  增长，撒谎的头部只会撞上 EOF 然后报错；另加单条目 4 GiB 硬上限。

判据：`unpack_in_blocks_symlink_escape`（造上述归档，断言文件**没有**落到
根外）、`unpack_in_still_allows_normal_nested_paths`（只测"挡得住"会让恒
失败的实现也变绿）、`lying_size_header_does_not_allocate`、
`absurd_size_header_is_rejected_outright`。

##### L8 `cp` 穿过 upper/rootfs 中间符号链接 `[Linux agent]` `[done]`

**已修**（门禁 CP.7、CP.8、AF.8）。`wbox cp`、`build` 的 `COPY`/`ADD` 目标、
`COPY --from` 的阶段内源、以及 `layers.rs` 的分层查找共用
`build::resolve_rootfs_path`，而那个函数原先是**纯词法**的：逐段消解 `..`、
拒绝逃出 rootfs，但**完全不看符号链接**。镜像/容器是外部输入，放一个
`/evil -> /home/someone`（宿主绝对路径）就够——`cp x <容器>:/evil/y` 词法上
老老实实待在 rootfs 里，落盘时宿主跟着链接走出去，在**宿主**上写了文件。

**做法**：新增 `wbox_codec::path::resolve_in_root`（`openat2(RESOLVE_IN_ROOT)`
的用户态版），逐段展开、链接目标重新以 rootfs 为根解析；`resolve_rootfs_path`
改为委托它。策略与 L12 的 guest VFS 一致（**跟随并重新展开**），与 L7 的
`tar::safe_join`（**见链接即拒**）有意不同——归档完全不可信，"跟着链接走"
没有合理语义；而镜像里的 `/etc -> /usr/etc` 是正常内容，该在容器内照常生效。

两处配套：`cp` 在"目标是目录、把源拷进去"时**重新解析**追加的那一段，而不是
直接 `join`（追加段本身可能是链接）；`copy_any` 递归拷目录前先摘掉已是链接的
目标，否则 `create_dir_all` 认的是链接指向的目录，整棵子树都从链接穿出去写。

**判据非空已验证**：同一套探针跑旧二进制（词法版），宿主目录里当场出现
`pwned` 文件；跑新二进制，宿主目录为空、相对链接爬根的那条明确报
「用 '..' 逃出了 rootfs」。反向用例 CP.5（正常嵌套路径照常拷得进去）与
`in_image_symlink_still_follows` 保证挡得住的同时没把功能一起挡掉。

##### L9 Linux native / Wine 共用后端验收 `[Linux agent]` `[done]`

**已取得失败断言。** 原条目是"验收当前失败，待取得失败断言"——真正的问题
不是用例红，而是 `test-wine-backend` 这个 **release 前置门禁在全 SKIP 时也返回 0**：
W 段的依赖检查刻意绕过 `WBOX_LBE_REQUIRE` 直接 `report SKIP`，于是 apt 装包
悄悄失败、或 wine 包名随发行版改了，这个 job 就**什么都没测却变绿**，还能放行发布。

这是同一个坑的第二个实例：脚本顶部早就记过一次 Ubuntu 24.04 起 AppArmor
默认关掉 unprivileged userns、脚本老实 SKIP 全部用例并返回 0 的情形。规律已
写进脚本顶部——**凡是"缺依赖就 SKIP"的段落，都要问一句：有没有哪个 job 的
全部存在意义就是跑这一段？** 有的话，那个 job 必须有开关把 SKIP 变成 FAIL。

两道判据都已落地（`scripts/test-linux-backend.sh` + `.github/workflows/ci.yml`）：

1. `WBOX_WINE_REQUIRE=1`（**只有** `test-wine-backend` 设）：wine/mingw 缺失
   记 FAIL 而不是 SKIP。不复用 `WBOX_LBE_REQUIRE` 是因为 wine 对
   `test-linux-backend` 确实可选，那里 SKIP 是对的。
2. **空跑兜底**：即便依赖装上了，W 段没让 PASS 计数涨过也判红。第 1 条只挡
   "依赖缺失"这一种成因，而空跑的成因不止一种（中间步骤失败、`if` 分支写歪），
   这条直接盯结果。

取证：不设开关时 `SKIP W.1-W.4`、PASS=232 FAIL=0、exit 0（本地行为不变）；
设 `WBOX_WINE_REQUIRE=1` 时两条 FAIL 都报出来、exit 1。两个开关的对照表见
`docs/testing.md`。

##### ~~L10 TLS 要不要也换成第一方~~ —— 已完成 `[done]`

结论是**做了**：`crates/wbox-tls` 是自实现的 TLS 1.3 客户端，`rustls` +
`rustls-rustcrypto` + `webpki-roots` 已从构建图移除。

保留这条记录，因为它的**取舍论证**仍然有效，且是评估同类问题的模板：
自己写 TLS 意味着自己写 X25519、AES-GCM、RSA/ECDSA 验签与 X.509 链校验，
那是未经审计、非常量时间的密码学。做的判断依据是三条，见 §2.2.2：
影响面只有 registry HTTPS（不涉及隔离）、镜像有独立于 TLS 的 digest 校验、
换掉的 provider 本来也是未审计的 alpha 版。

取证：`wbox pull alpine:3.20` 走完整自实现栈从 Docker Hub 拉通，
manifest digest 与换之前**逐字节一致**；随后 `wbox run` 起容器执行 busybox。

##### L12 guest VFS 的宿主符号链接逃逸 `[Linux agent]` `[done]`

**已修，曾是 Critical。** 由 Windows 侧 agent 的代码审查指出，也是
`tests/known-failures.txt` 里唯一带安全含义的一组（H 组）——代码里原本自己
注释着"已知缺口"。

**洞**：`crates/wbox-linux` 的 VFS 只做词法规范化，挡得住 `../../etc/passwd`，
却完全挡不住符号链接。rootfs 里一个 `/evil -> /` 的链接，guest 打开
`/evil/etc/shadow` 时词法上一路合法，内核在宿主上跟着链接走，直接读到宿主的
`/etc/shadow`。guest 套件实测确认：修复前 `t_sec_path` 报的就是
"SANDBOX ESCAPE: open(...) succeeded"。

**修法与残余风险**见 F4.R4 下的"宿主符号链接逃逸"段与
`docs/rust-rewrite.md` §4 第 9 条。要点：逐段解析、链接目标重新从 rootfs 根
展开，结构上不可能指到 prefix 之外；不用内核 `openat2(RESOLVE_IN_ROOT)` 是
因为它只在 Linux 5.6+，而本 crate 也要在 Windows 宿主上跑。

**同一类问题的第二处**（审查另列 High，一并修掉）：卷挂载点的
`create_dir_all` 会跟着镜像里的符号链接走，把目录建到镜像别处乃至宿主上，
随后的 bind mount 也就挂到了那个位置。改成复用 `wbox_codec::tar::safe_join`，
末段另判不得为符号链接（`src/backend/linux_ns.rs`）。

**基线随之收紧**：guest 套件 5 PASS → 7 PASS，`t_sec_path` 与 `t_sec_linkabs`
移出 `tests/known-failures.txt`，H 组整组清空，四条安全用例全部在基线之外。
判据非空已验证——临时退回旧实现，新增用例当场读到宿主文件内容而变红。

**与 L7 的分工**：L7 修的是**归档解包**（`load`/`import`/`ADD`）的同形态洞，
本条修的是 **guest 运行期**的路径解析，第三个入口 L8（`cp`/`COPY` 的目标
路径）也已修完。三处的策略分两档，是有意的：归档完全不可信，见链接即拒
（L7）；镜像与容器内容里的链接是正常内容，跟随但重新以 rootfs 为根展开
（L12、L8）。

##### W5 Q2 的端口映射 `-p` 取证 `[Linux agent]` `[done：结论是语义不适用]`

**结论：Windows 宿主上 `-p` 没有可兑现的语义，应保持明确拒绝。**

取证靠读模拟器源码即可，不需要 Windows 机器。当年读的是 vendored 的
blink C 源码（`blink/hostfs.c` 里 `HostfsSocket`/`HostfsBind`/`HostfsListen`
都是直接转调宿主同名 API）；那份 C 实现已被 `crates/wbox-linux` 取代，
但结论不变，且现在更彻底——**Rust 模拟器连 socket 族都还没实现**
（`syscall/mod.rs` 对 socket 相关调用返回 `-ENOSYS`，见
`docs/rust-rewrite.md` §4）。两代实现都没有自建网络栈
（无 slirp / usermode TCP 之类）。

于是 **guest 的 socket 就是宿主的 socket**：Linux guest 在 Windows 上
`bind(0.0.0.0:80)` 绑的就是 Windows 的 80 端口。既然容器端口本来就是宿主端口，
"把宿主 8080 映射到容器 80"在这一格里不成立——它退化成同一台机器上的 8080→80
转发，与容器无关，用户自己起个转发器即可。

**由此还澄清了一件更要紧的事**：Q2 的网络隔离模型与 Q3 **根本不同**。
Q3 靠 network namespace（容器有独立网络栈，默认空 netns）；Q2 靠 AppContainer
不授 `INTERNET_CLIENT` 能力——是能力开关，不是独立网络栈。两者默认都断网，
但强度与形态不一样，§2.4 Q2 已补上这一行。想在 Q2 得到 netns 级隔离，只能给
模拟器加一层用户态网络栈，那是另一个数量级的工作。

保持现状（Windows 上 `-p` 明确报错）是对的，但报错文案已按这个结论修正：
不是"Windows 没有对应原语"，而是"guest 端口即宿主端口，没有可映射的东西"。
说错原因会让人以为换个实现就能做。

## 5. 非功能需求

### N1 可移植性

- Rust stable；Windows 主目标 `x86_64-pc-windows-msvc`。
- Windows 发布物当前是**两个**纯 Rust exe：`wbox.exe`（CLI/supervisor）与
  `wbox-linux.exe`（Linux guest 引擎）。同一份 `crates/wbox-linux` 同时产出
  bin 形态（`EmuBackend` 用）与 lib 形态（`src/runtime` 的进程内入口），
  不是两份实现。**合并成单文件是 §4.9 R8 的待决项**，卡点是"合并后
  AppContainer 套模拟器的双层隔离怎么算"，不是 Rust-only 约束——两个 exe
  都是纯 Rust，不违反 §2.2.1。
- 不引入后台服务、驱动、tokio 或 clap。
- release 保持免安装、可直接复制；完整 Windows 包为 `wbox.exe` +
  `wbox-linux.exe` + `SHA256SUMS.txt`，不含任何运行时 DLL（`WP.4` 盯这条）。

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

- Rust-only 门禁扫描仓库源码、构建脚本、Cargo native 依赖和发布物导入表；发现
  C/C++ 编译、链接或随包分发即失败。
- Win32 unsafe 调用集中在平台模块，并说明 Safety 前提。
- CLI 帮助、错误码、功能状态和测试基线各有唯一事实源。
- 行为修复必须带最小回归；共享 fd、进程、映射或路径逻辑需覆盖 fork/失败回滚。
- 禁止用固定已知失败掩盖已修复行为，基线变好时必须同步收紧。

## 6. 当前状态

状态日期：2026-07-29，分支：`main`，版本：`1.0.0-rc2` 后续滚动开发。

| 工作流 | 状态 | 最近可信信号 |
|---|---|---|
| Windows 原生容器 | active | WN.1-WN.8、WNET.1-WNET.4、WP.1-WP.27 进入门禁；私有临时目录、package LOCALAPPDATA 与 Job 总内存超限行为均已实机核验 |
| OCI pull/cache/config | active | BusyBox 1.36 与 Debian bookworm-slim 实机运行 rc0；失败 pull 后旧 BusyBox 缓存继续运行 rc0，原子交换与回滚另有 G0 失败注入 |
| Windows Linux guest | active | `crates/wbox-linux` 纯 Rust runtime 已接管 WP.3/WP.3E/WP.3W；Ubuntu 24.04 的 Bash/APT/dpkg/getconf/uname 已进入固定摘要 WU.1/WU.2 产品门禁，完整 guest ABI 缺口仍以 guest C 套件为准 |
| Windows shell 矩阵 | active | 纯 Rust runtime 已覆盖 BusyBox shell/fork/exec/管道；完整 guest ABI 缺口以 `tests/known-failures.txt` 为准 |
| Rust 主机逻辑 | G0 complete | Windows workspace 单测与 Win32 实机模块持续进入 CI；实时数量以 runner 输出为准 |
| Linux 原生后端 | active | 主路径 G3 已覆盖；资源溢出、失败清理和跨后端语义待补 |
| Linux Wine 路径 | active | PE 分派/退出/网络 G3；资源超限行为待补 |
| 后台生命周期管理 | complete | Linux P.6-P.25 与 Windows WP.6-WP.24（含 create/start/rename、READY/ERROR、kill/top、管道 EOF）已进门禁；Windows OCI/模拟器 exec 明确不支持 |
| Rust-only 依赖约束 | done | `Cargo.lock` 16 条 = 5 个第一方 crate + `libc`/`windows-sys` 及其 target 垫片；三条棘轮（`src/runtime/mod.rs`）盯住 C 源码、被换掉的 crate 与整棵依赖图 |

上述数字是该日期的状态快照，不作为门禁配置。真实基线分别以测试 runner、
`tests/known-failures.txt` 和 `.github/workflows/ci.yml` 为准。

Windows Linux guest 的发布门禁只接受 `crates/wbox-linux` 产出的纯 Rust
`wbox-linux.exe`；Blink 时代取证仅保留为历史参考，不属于当前能力证据。

## 7. 里程碑与时间线

```text
2026-07-23
└── 验证用户态模拟路线，确定 Windows 上运行 Linux ELF 的架构

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

2026-07-28
├── Rust-only 成为产品与发布硬约束
├── 撤回 Windows volume 的 Blink/C brokerfs 实验
├── 纯 Rust Linux ELF/OCI runtime 替换**完成**：vendor/blink 删除，
│   引擎换成 crates/wbox-linux（第一档达成）
├── 第二档收紧：serde_json/sha2/base64/flate2/tar/anyhow/ureq/rustls
│   全部换成第一方，含自实现 TLS 1.3；构建图 119 -> 16 个 crate
└── 安全收口：归档解包符号链接越界（L7）、guest VFS 宿主符号链接逃逸（L12）、
    卷挂载点跟随符号链接、`-v :ro` 未递归到子挂载
```

下一里程碑不使用虚构日期，按验收条件推进：

1. `[done]` Linux cgroup v2 改为兄弟 leaf（父级不可写时退回 supervisor/target
   双 leaf），CI 现造委派子树做门禁，已取得实际限额证据。
2. `[done]` F4.R0–F4.R4：C/C++ 依赖已清零，纯 Rust ELF64 loader、初始栈、
   x86-64 整数指令全集 + SSE/SSE2、76 条 syscall 与 VFS 前缀约束均已落地
   并有门禁（173 项）。进程族（快照式 fork/execve/wait4）已补齐，Windows
   产品用例 WP.3W 随之转绿；VFS 的宿主符号链接逃逸已封死（§4.9 L12）。
   下一步是 §4.9 R8（单一 exe）与 x87/socket/MAP_SHARED 三个缺口。
3. `[planned]` 决定是否发布新的 rc；要求全部发布门禁通过且 PRD 状态同步。
4. `[done]` Windows stop、原生 exec、Job 总内存、W3 写路径取证与 W7 ACL
   粒度均已进入门禁；后续按 §4.9 W8 继续做 capability 取证。

## 8. 验收与发布

常规改动至少执行相关单测。跨平台、公共 fd/进程/路径逻辑或发布改动必须检查：

```text
release gate
├── test-linux                 Rust/Linux
├── test-windows               Rust/Windows 真机
├── check-windows-msvc         双目标 clippy/check
├── smoke-windows              AppContainer + Job 启动链
├── build-wbox-linux           纯 Rust ELF loader/CPU/syscall 组件与发布物门禁
├── guest-tests                预构建 ELF fixtures 的 Rust runtime 行为门禁
├── test-windows-product       release wbox.exe + wbox-linux.exe 的 Windows/OCI 产品路径
├── test-linux-backend         namespace/network/resource/lifecycle
└── test-wine-backend          Linux 隔离层内运行 PE
```

Tag 发布必须等待所有 required jobs 成功，并产出不含 C/C++ runtime 的
`wbox.exe`、portable zip 和 `SHA256SUMS.txt`。完整命令和 SKIP 规则见
`docs/testing.md`。

## 9. 需求变更规则

- 新功能先在本文加入场景、边界、功能节点和可测试验收，再实现。
- 修复不需要新增产品节点，但若改变支持范围或限制，必须更新对应节点。
- 状态从 `[active]` 改为 `[done]` 必须能指向自动测试或可重复真机记录。
- 不把一次性的调试过程堆进本文；原因与修复写 commit/CHANGELOG，长期操作知识
  写技术参考。
- 不在 README、架构手册和测试手册复制“当前进度”。它们只链接本文。
