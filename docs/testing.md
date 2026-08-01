# wbox 测试与发布手册

本文记录可执行验证方式和门禁规则。当前进度与最近基线见 `../PRD.md`。

## 1. 测试分层

```text
G0 Rust tests
├── CLI、目标分类、环境合并
├── OCI 引用/认证/digest/解包/config
├── Linux 后端纯逻辑
└── Windows profile/Job/启动链（仅 Windows 编译与运行）

G1 guest C tests (`tests/run.sh`)
└── wbox-linux 内的 Linux syscall 与 fork/exec 行为

G1/G3 shell matrices
├── `scripts/test-matrix.sh`: Windows/wine 上的 wbox-linux 场景
└── `scripts/test-linux-backend.sh`: Linux namespace、资源、网络、Wine

G3/G4 product paths
└── `scripts/test-windows-product.ps1`: 最终双 exe + 离线 OCI fixture
```

G0/G1 通过不能推导 G3/G4 通过。测试层级、完成定义和逐需求追踪矩阵见
`PRD.md` §4.0。纯解析改动运行相关 Rust 测试；公共 fd、进程、内存、路径、
OCI 解包或后端行为改动应运行完整相关层，并依赖 CI 补齐其他宿主。

## 2. 常用命令

### 2.1 Rust

```powershell
# 日常反馈：静态语法、格式、host clippy、库测试
scripts/check.ps1 -Quick

# 提交前：再执行 workspace 全部 Rust tests
scripts/check.ps1

# Linux 宿主审查 Windows 专属代码时显式加入交叉目标
scripts/check.ps1 -Quick -WindowsTarget
```

`scripts/lint.ps1 -Mode Static` 只检查工作树空白、PowerShell AST 和 JSON 语法，
不获取 Cargo 构建锁，适合后台只读观察者调用。`-Mode Rust` 运行 rustfmt 与 host
Clippy；`-WindowsTarget` 再加入 `x86_64-pc-windows-msvc` Clippy。Linux host
不会编译 `cfg(windows)` 模块，所以跨宿主修改共享/Windows 代码时双目标都是门槛。
工作树空白检查同时覆盖未暂存与已暂存 diff，避免 `git add` 后反而漏过错误。

验证按成本递增：先 `lint.ps1 -Mode Static`，再 `check.ps1 -Quick`，然后运行改动
所属的 G2/G3 产品门禁；`check.ps1` 和完整 CI 留给提交前。不要同时启动多个 Cargo
门禁争抢同一个 `target` 构建锁。wrapper 只打印阶段、失败和耗时摘要，详细产品
证据仍由下述唯一 owning gate 产出。`check.ps1` 默认清理 incremental 与测试临时
目录；需要为连续本地迭代保留热缓存时显式传 `-KeepIncremental`。构建 wrapper
同样默认清 incremental，满足阶段性交付后的磁盘清理要求。

CI 固定 Rust 1.97.1。升级编译器是独立变更：先跑 Quick 和双目标 Clippy，再观察
完整 CI，不能让浮动 `stable` 在普通功能提交中突然改变 lint 规则。本地暂不放
精确 channel 的 `rust-toolchain.toml`：只有 `stable` 别名的离线机器会为同版本
再次联网安装并卡住；提供离线工具链镜像后再统一本地 pin。

日常构建使用仓库 wrapper，而不是长期直接调用 `cargo build`：

```powershell
# Windows
scripts/build.ps1
scripts/build.ps1 -Release
scripts/build.ps1 -Release -Package wbox-linux
```

```bash
# Linux
scripts/build.sh
scripts/build.sh --release -p wbox-linux
```

wrapper 无论构建成功还是失败都会执行 `scripts/cleanup-target.*`，删除所有 target
三元组下可再生成的 `incremental/`，以及根目录的 `tmp/`、`review-*`、`*.tmp`、
`*.part` 临时状态。`deps/`、`build/`、`.fingerprint/` 是 Cargo 用于避免全量重编的
有效缓存，和最终二进制一样保留；不能因为文件多就当成垃圾删除。Windows 可传
`-KeepIncremental`，Linux 可设置 `WBOX_KEEP_INCREMENTAL=1` 临时保留增量缓存；
不得把该选项写入 CI 或长期开发命令。

测试不得直接并发修改进程环境。使用 `crate::testenv::EnvGuard`；需要临时 HOME
时使用 `cli::TempHome`，额外变量经其同一守卫设置，避免重入锁。

### 2.2 wbox-linux guest

前置：

- 已构建的 `target/release/wbox-linux.exe`（`cargo build --release -p wbox-linux`）。
- Zig，用于把 `tests/guest/t_*.c` 编译为静态 x86-64 Linux ELF。
- Windows 上使用 Git Bash/MSYS2 运行脚本。

```bash
WBOX_LINUX=target/release/wbox-linux.exe bash tests/run.sh
```

单项可直接运行构建后的 guest ELF。构建与 runner 细节见
`tests/guest/build.sh` 和 `tests/run-guest-tests.sh`。

### 2.3 Windows/wine 场景矩阵

```bash
scripts/test-matrix.sh target/release/wbox-linux.exe ./busybox
```

常用环境：

- `WBOX_MATRIX_TIMEOUT=<seconds>`：单项超时，`0` 关闭。
- `WBOX_MATRIX_NET_SKIP=1`：跳过依赖公网的下载项。
- `WBOX_GUEST_SKIP=1`：矩阵不重复运行专职 guest 套件。

### 2.4 Linux 后端

```bash
scripts/test-linux-backend.sh target/debug/wbox ./busybox
```

需要静态 busybox 和 unprivileged user namespace。安装 Wine 和 MinGW 后脚本会
运行 W 段 PE 夹具。

两个 `*_REQUIRE` 开关，各管一类依赖，**都是把 SKIP 变成 FAIL**：

| 开关 | 管什么 | 谁必须设 |
|---|---|---|
| `WBOX_LBE_REQUIRE=1` | userns、cgroup 等 wbox 自身的硬前置 | `test-linux-backend`、`test-wine-backend` |
| `WBOX_WINE_REQUIRE=1` | wine 与 mingw（W 段的可选依赖） | 仅 `test-wine-backend` |

普通本地环境两个都不设，能力缺失记 SKIP。

**为什么需要第二个开关**：wine/mingw 对大多数场景确实可选，`test-linux-backend`
不装它们、记 SKIP 是对的。但 `test-wine-backend` 这个 job 的**全部存在意义就是
跑 W 段**——那里缺依赖只可能是装包失败或包名随发行版改了。不设的话，一个
required job 会在"什么都没测"的情况下变绿，而它还是 release 的前置门禁。

脚本另有一条兜底：`WBOX_WINE_REQUIRE=1` 时如果 W 段一条断言都没产出，直接判红
——挡的是"依赖装上了，但中间某步失败导致整段空跑"这类成因。

这两条都源自同一个教训（脚本顶部有完整记述）：**凡是"缺依赖就 SKIP"的段落，
都要问一句——有没有哪个 job 的全部存在意义就是跑这一段？** 有的话，那个 job
必须有开关把 SKIP 变成 FAIL。

cgroup v2 的存在不等于当前进程获得委派。诊断时同时记录：

- `/proc/self/cgroup`。
- 当前层级的 `cgroup.controllers`、`cgroup.subtree_control`。
- 能否创建子 cgroup、下发控制器和写入目标限制文件。
- 进程实际位于哪个 leaf。

`scripts/probe-cgroup2.sh` 是取证脚本，不替代产品验收。

### 2.5 Windows 产品路径

该门禁不访问 registry，使用仓库内静态 `busybox` 构造本地镜像缓存，并只复制
`wbox.exe` 与 `wbox-linux.exe` 到临时 bundle：

```powershell
scripts/test-windows-product.ps1 `
  -Wbox target/release/wbox.exe `
  -WboxLinux target/release/wbox-linux.exe `
  -Busybox busybox
```

它覆盖 Windows 原生 workload、环境过滤、正常退出状态清理，以及从
`wbox run <image>` 到 AppContainer、纯 Rust `wbox-linux` 和 Linux ELF 的完整路径。任何前置
缺失或执行失败都直接 FAIL，不允许 SKIP。

detached 生命周期门禁还覆盖 READY/ERROR：父进程只有在真实 workload 创建并恢复后
才能返回容器名；缺失程序与失败 pull 必须返回原始错误和非零退出码。另有
`create -> rename -> start` 真机路径，防止保存配置中的旧名称复活。

`WP.25` 覆盖 Windows `--private-tmp` 的实际落盘语义：状态目录中的 `TMPDIR`
与 AppContainer package 专属的 `TEMP`/`TMP` 都必须可写，且显式
`-e TMPDIR=...` 不能被默认值覆盖。

`WP.26` 对照运行同一内存分配 workload：无限额时必须完成，`--memory 64`
时必须捕获 OOM，防止 Job 参数“设置成功”被误当成真实限额证据。

`WP.27` 验证普通非 UWP 程序的 `LOCALAPPDATA` 位于 AppContainer package
专属 `AC` 且可写，并断言写入不落到宿主真实 LocalAppData。一次性的
VirtualStore 边界可用纯 Rust i686 探针复核：

```powershell
scripts/probe-windows-virtualstore.ps1 -Wbox target/release/wbox.exe
```

Ubuntu 24.04 使用独立门禁。Linux CI 先从固定 linux/amd64 manifest digest
生成最小运行 fixture；Windows job 下载后离线执行 glibc、Bash、APT、dpkg、
getconf 和退出码透传：

```powershell
scripts/test-windows-ubuntu.ps1 `
  -Wbox target/release/wbox.exe `
  -WboxLinux target/release/wbox-linux.exe `
  -UbuntuImage <fixture-image-directory>
```

`WU.1/WU.2` 都是 required 产品门禁：fixture 来源、AppContainer、动态链接或
guest ABI 任一失败都直接 FAIL，不允许 SKIP。`wbox pull` 的 registry/TLS
门禁与该离线 ABI 门禁分开，避免公网故障掩盖 Windows guest 回归。

### 2.6 Windows 原生程序矩阵

```powershell
scripts/test-windows-native.ps1 -Wbox target/debug/wbox.exe
```

该矩阵只使用 Windows 自带程序与本地临时目录，不依赖公网。它覆盖 `cmd.exe`、
Windows PowerShell 解释器/CLR、`hostname.exe`、`whoami.exe`、子进程、显式可写工作目录、
退出码转发和 `ps --all` 状态清理。网络拒绝/放行属于独立行为门禁，不以该矩阵
通过替代。

`WN.2` 使用 CLR 输出验证解释器本体。GitHub Server 2025 的默认 AppContainer
目前不能自动发现 `Write-Output` 所在的宿主 PowerShell 模块；因此该矩阵不代表
所有标准 cmdlet 已兼容，模块加载需单独建立跨宿主行为门禁。

`cargo test` 中的 Win32 真机模块还直接以 AppContainer 子测试进程验证 ACL：
RX 目录必须可读，覆盖和新建必须返回 `PermissionDenied`。这验证授权粒度，
不把 Windows 原生 `-v` 误报为已支持。

### 2.7 Windows 网络行为门禁

```powershell
scripts/test-windows-network.ps1 -Wbox target/debug/wbox.exe
```

该门禁先证明宿主可访问一个公网数值 IP 端点，再用同一端点断言默认
AppContainer 被拒绝、`--allow-network` 成功，并检查两次运行的状态清理。
数值 IP 用于隔离 capability 行为与系统 `curl.exe` 域名 resolver 辅助线程问题；
DNS 放行已由 `nslookup` 实机验证，但尚未纳入此门禁。

W8 的 private-network capability 不能用同机地址验证，因为 AppContainer
loopback isolation 会额外阻断。需在另一台私网机器启动 HTTP 服务后运行：

```powershell
$env:WBOX_TEST_PRIVATE_ENDPOINT = "http://PRIVATE-PEER:PORT/"
cargo test private_network_capability_controls_external_endpoint -- --ignored --nocapture
```

### 2.8 后台观察

后台 subagent 只做增量、只读观察：检查新提交、CI 阶段变化、测试日志新增失败和
`target` 体积趋势；不得改共享热文件、启动第二个 Cargo 构建、启动 GUI/公网产品
测试或周期性重读整份日志。报告至少包含观察到的 HEAD、失败 gate、最小错误片段
和建议的 owning gate。主 agent 负责跨模块决策、最终串行验证以及小而完整的提交。

同一 `HEAD + 命令` 已在运行时只观察现有运行，不重复启动。GitHub Actions 使用
同 workflow/ref 的 concurrency group，后续 push 会取消过时运行，避免并发烧掉
runner 时间并让旧结果干扰判断。

## 3. 已知失败基线

机器可读基线是 `tests/known-failures.txt`，说明与修复历史在
`tests/KNOWN-FAILURES.md`。

runner 规则：

| 结果 | 裁决 |
|---|---|
| 实际失败完全等于适用基线 | 通过并打印已知失败 |
| 出现基线外失败 | 失败，属于回归 |
| 基线内项目变为通过 | 失败，要求同步收紧基线 |

基线可用 `@native`、`@wine` 表示环境专有差异。
`WBOX_GUEST_NO_BASELINE=1` 可查看原始结果。确定性缺陷不得用 SKIP 或
`should_panic` 掩盖。

## 4. 超时、网络与 SKIP

- 可能阻塞的 guest/fork/socket 测试必须有 runner 超时。
- registry 或公网下载不可达可以 SKIP，并清楚打印原因。
- 编译器、artifact 或宿主能力缺失仅在非 required 环境允许 SKIP。
- 专门配置的 CI job 必须使用 REQUIRE 语义，避免“全绿但零覆盖”。
- 本地构造输入能验证的错误路径不得依赖公网，也不得 SKIP。

## 5. CI 门禁

`.github/workflows/ci.yml` 是 job、触发条件和 required 关系的唯一事实源。
当前职责：

| Job | 覆盖 |
|---|---|
| `preflight` | PowerShell/JSON/空白、rustfmt、host all-targets check（Rust 1.97.1） |
| `test-linux` | Linux Rust tests |
| `test-windows` | Windows Rust tests，包括 Win32 专属模块 |
| `check-windows-msvc` | host 与 Windows target lint/check |
| `smoke-windows` | CLI、AppContainer、Job 和可选 pull 冒烟 |
| `build-wbox-linux` | UCRT64 构建及 Windows 场景矩阵 |
| `guest-tests` | guest C 回归与基线裁决 |
| `test-windows-product` | 最终双 exe 的 Windows 原生与 OCI Linux guest 产品路径 |
| `test-linux-backend` | Linux 原生后端 |
| `test-wine-backend` | Linux 隔离层内的 Windows CLI |
| `release` | tag 全绿后打包与发布 |

PR 可以只跑快速核心组；`main` push、tag、nightly 和手动触发运行完整矩阵。
昂贵的 Cargo/产品 job 先依赖 `preflight`，确定性静态失败不会继续占用 Wine、
Windows release 和 backend runner。

## 6. 发布

Tag 发布前确认：

1. `PRD.md` 的范围、状态和限制与代码一致。
2. 所有 required jobs 成功，不能以本地结果替代缺失宿主。
3. guest 已知失败基线没有过期。
4. Release 包含 `wbox.exe`、`wbox-linux.exe`、
   `wbox-portable-windows-x64.zip` 和 `SHA256SUMS.txt`。
5. 用户可见变更已写入 `CHANGELOG.md`。

rustfmt 已纳入 `scripts/lint.ps1`；格式失败时只格式化本次涉及的 Rust 代码，不借机
做无关的大范围重排。
