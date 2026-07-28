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
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo clippy --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings
```

双目标 clippy 都是门槛；Linux host 不会编译 `cfg(windows)` 模块。

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
三元组下可再生成的 `incremental/`，以及根目录的 `tmp/`、`review-*` 测试临时状态。
`deps/`、`build/`、`.fingerprint/` 和最终二进制会保留，避免每次退化成全量重编。
Windows 可传 `-KeepIncremental`，Linux 可设置 `WBOX_KEEP_INCREMENTAL=1` 临时保留
增量缓存；不得把该选项写入 CI 或长期开发命令。

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

### 2.7 Windows 网络行为门禁

```powershell
scripts/test-windows-network.ps1 -Wbox target/debug/wbox.exe
```

该门禁先证明宿主可访问一个公网数值 IP 端点，再用同一端点断言默认
AppContainer 被拒绝、`--allow-network` 成功，并检查两次运行的状态清理。
数值 IP 用于隔离 capability 行为与系统 `curl.exe` 域名 resolver 辅助线程问题；
DNS 放行已由 `nslookup` 实机验证，但尚未纳入此门禁。

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

## 6. 发布

Tag 发布前确认：

1. `PRD.md` 的范围、状态和限制与代码一致。
2. 所有 required jobs 成功，不能以本地结果替代缺失宿主。
3. guest 已知失败基线没有过期。
4. Release 包含 `wbox.exe`、`wbox-linux.exe`、
   `wbox-portable-windows-x64.zip` 和 `SHA256SUMS.txt`。
5. 用户可见变更已写入 `CHANGELOG.md`。

`cargo fmt --check` 当前可能显示 vendor/历史文件的大量既有格式差异。除非任务
明确要求格式化，不做跨仓库批量改写；只检查本次修改没有引入无关代码变化。
