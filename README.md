# wbox — portable Windows 进程容器

`wbox` 把任意 Windows 原生进程关进一个由 **AppContainer（低完整性令牌）+ Job Object（资源限额与生命周期收割）** 构成的进程级隔离单元。

**定位**：面向**未启用硬件虚拟化**（无 VT-x/AMD-V → 无 WSL2、无 Hyper-V 隔离）的 Windows 10/11 / Server 环境。单 exe、免安装、默认**不需要管理员权限**、不需要启用任何 Windows 可选功能。它不是 Docker 替代品——没有镜像分层、没有内核命名空间，只做"进程级容器"。

> **测试声明**：本项目在 Linux 沙箱中完成开发，已通过 `cargo check --target x86_64-pc-windows-msvc` 编译期验证。**Windows 真机冒烟已通过**：Windows Server 2022 上 `wbox run` 的 AppContainer profile 创建 + Job Object 分配 + 子进程退出码转发全链绿（`write.exe` 等系统自带程序 rc=0）；Job 嵌套、不同 Windows 版本上的 profile 行为差异仍需用户在目标环境复测。

## 构建

```powershell
rustup target add x86_64-pc-windows-msvc   # 如已安装可跳过
cargo build --release
# 产物：target/release/wbox.exe（单文件，可拷贝到任意目录直接运行）
```

### Linux 交叉构建（x86_64-pc-windows-gnu，无需 MSVC）

```sh
pip install ziglang                     # 提供 zig cc 作为 mingw 链接器
rustup target add x86_64-pc-windows-gnu
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=$PWD/win32-link-zig.sh \
  cargo build --release --target x86_64-pc-windows-gnu
# 产物：target/x86_64-pc-windows-gnu/release/wbox.exe
```

注意：rustc 会向 linker 透传 `-nodefaultlibs`，zig lld 的 no_fallback 模式
因此找不到 libmsvcrt.a / libwindows.*.a；`win32-link-zig.sh` 用 bash 数组
重建参数列表过滤该 flag（循环里直接 continue 改写 "$@" 不生效），其余参数
原样透传给 `zig cc -target x86_64-windows-gnu`。release profile
（opt-level=z / lto / codegen-units=1 / panic=abort / strip）已按最小体积配置。

## 用法

> 以 `wbox --help` 的输出为准（唯一文本源在 `src/cli/mod.rs` 的 `USAGE`）。

```
wbox run [OPTIONS] -- <CMD> [ARGS...]            运行本地 Windows 程序
wbox run [OPTIONS] <IMAGE> [-- <CMD> [ARGS...]]  运行已 pull 的 OCI 镜像（Linux 后端）

  --name <NAME>     容器名（AppContainer profile 名），默认 "wbox-<pid>"（1..=64 字符）
  --memory <MB>     每进程内存上限（MB），0 = 不限，默认 0
  --cpu-pct <N>     CPU 硬性百分比上限 1-100（Job Object CPU rate control），默认 0 = 不限
  --max-procs <N>   最大进程数，默认 0 = 不限
  --allow-network   授予 INTERNET_CLIENT capability（默认不授予网络能力）
  --no-network      显式声明不授予网络（默认行为，预留）
  --workdir <DIR>   容器工作目录（"镜像根"），默认当前目录（仅原生模式）
  --keep-profile    退出后保留 AppContainer profile（默认删除）
  --rm              显式声明退出即清理（默认行为，docker 习惯写法）
  --interactive     连接 stdio（当前默认且唯一支持的模式；--detach 预留）
  --pull            run 目标为镜像时，本地无缓存则先 pull
  --env-pass-all    继承完整宿主环境（默认仅白名单；BLINK_*/WBOX_* 保留键始终不透传）
  -V, --verbose     打印隔离配置摘要
```

**子进程环境**：默认**不透传**宿主环境，只给一份最小白名单（`SystemRoot` 等），
避免把宿主机密（如 `WBOX_REGISTRY_PASS`）带进容器；`--env-pass-all` 可放开，
但 `BLINK_*` / `WBOX_*` 这类隔离旋钮始终剥离，镜像 Env 里的 `BLINK_PREFIX`
也会被强制覆盖为 rootfs。

示例：

```powershell
# 内存 256MB、CPU 硬上限 50%、最多 32 个进程，无网络
wbox run --memory 256 --cpu-pct 50 --max-procs 32 -- cmd.exe /c echo hello

# 指定"镜像根"目录并允许网络
wbox run --name web --workdir C:\images\webapp --allow-network -- webapp.exe --port 8080

# 查看隔离配置（GUI 程序在 AppContainer 下行为受限，调试建议用控制台程序）
wbox run -V --keep-profile -- cmd.exe /c whoami /all
```

**当前能 / 不能**（避免踩坑先看这里）：

| 场景 | 状态 |
|---|---|
| 控制台程序（cmd/powershell/node/python 等 CLI） | ✅ 设计目标场景 |
| 资源限额（内存/CPU/进程数）+ 进程树收割 | ✅ |
| 默认断网、`--allow-network` 放行 | ✅ |
| 拉取 OCI 镜像 rootfs（`wbox image pull ubuntu:24.04`） | ✅ |
| **运行 Linux 镜像**（`wbox run ubuntu:24.04 -- bash`） | 🔨 全链路已串联：镜像解析 → rootfs 定位 → config 合并（Entrypoint/Cmd/Env）→ 注入 resolv.conf 与 `BLINK_PREFIX` → AppContainer 内拉起 wbox-linux.exe。Wine 11.11 与真 Windows CI 已验证 busybox 静态程序、动态 glibc ls/cat/bash/uname/apt、wget、epoll/socket/AF_UNIX 和快照式 fork；shell 管道/命令替换/后台任务、fork 子 DNS 与 `apt-get update` 均实测通过。剩余限制为宿主异步信号、EPOLLET、glibc pthread/clone、mremap 扩容和 ptrace；详见 vendor/blink/WIN32-PORT.md §0。 |
| GUI 桌面程序（notepad 等） | ⚠️ AppContainer 对 GUI 有天然限制，多数会失败或异常，非目标场景 |
| Windows 服务 / COM / 驱动类程序 | ❌ 超出进程级容器边界 |
| 容器生命周期管理（ps/stop/rm/logs/exec） | 📐 未实现（v1 为前台一次性运行） |

**退出码**：子进程退出码原样转发；wbox 自身错误：`1`=参数错误、`2`=AppContainer profile 错误、`3`=Job Object 错误、`4`=进程创建错误、`5`=Registry/镜像错误。

## OCI 镜像拉取（image 子命令）

不依赖 docker，直接实现 OCI Distribution Spec v2 客户端，把镜像 rootfs 解到本地缓存：

```
wbox image pull <REF> [--os linux] [--arch amd64] [--registry <HOST>] [-V]
wbox image list                 列出本地缓存的镜像
wbox image show <REF>           打印已 pull 镜像的 config 摘要（敏感值脱敏）
wbox image rm <REF> [--yes]     删除本地缓存（默认交互确认，--yes 跳过）
```

- **引用补全规则**：`ubuntu:24.04` → `registry-1.docker.io/library/ubuntu:24.04`；无 tag 默认 `latest`；显式前缀如 `quay.io/prometheus/busybox:latest` 直接识别。
- **认证**：匿名 Bearer token 流程（解析 `WWW-Authenticate` → realm 取 token → 重试）；私有 registry 可用环境变量 `WBOX_REGISTRY_USER` / `WBOX_REGISTRY_PASS`（Basic 认证走 token 端点）。
- **manifest list**：按 `--os/--arch` 选择子 manifest（默认 `linux/amd64`，Windows 宿主同样拉 Linux rootfs）。
- **完整性**：所有"按 digest 取回的内容"（index 本体、从 manifest list 选出的子 manifest、config、每层 blob）均做 sha256 digest 校验，不匹配即失败退出（退出码 5）。
- **解包**：支持 gzip tar / 纯 tar 层（zstd 等压缩格式明确报错），按序应用；处理 whiteout（`.wh.<name>` 删除下层文件、`.wh..wh..opq` opaque 目录在解包本层条目之前统一清空）；硬链接两遍处理。
- **解包安全**：除拒绝 `..`/绝对路径条目外，所有写出/删除路径都经逐段符号链接解析（仿 Docker `FollowSymlinkInScope`）——任何中间组件是指向 rootfs 之外的 symlink（绝对目标或 `..` 逃逸）即跳过该条目；symlink 条目目标本身越出 rootfs 时也拒绝创建。
- **符号链接降级**：Windows 默认无 `SeCreateSymbolicLinkPrivilege`，symlink 条目创建会失败；此时延迟记录并在层末把**目标内容复制**到链接位置（文件复制、目录递归复制）。语义差异：降级后链接不再是引用，后续层对目标的更新不会反映到副本，占用空间也略增；启用开发者模式或以管理员运行即可创建真实符号链接。
- **blob 重试**：层/config 拉取对网络错误与 5xx 做 3 次指数退避重试。
- **缓存目录**：`%USERPROFILE%\.wbox\images\<registry>\<name>\<tag>\`（Linux/macOS 为 `~/.wbox/images/...`），内含 `rootfs/`、`manifest.json`、`config.json`、`layers.json`；缓存键含 registry，路径段中的 `:`（端口、digest 引用）替换为 `_` 以兼容 Windows 文件名；重复 pull 同一引用前会先清空旧 `rootfs/`。
- **镜像加速**：Docker Hub 不可达时可用 `--registry docker.m.daocloud.io` 等 mirror，或直接引用 `quay.io/...`。

示例：

```powershell
wbox image pull ubuntu:24.04                 # 拉取 linux/amd64 ubuntu rootfs
wbox image pull hello-world --registry docker.m.daocloud.io
wbox image pull quay.io/prometheus/busybox:latest -V
wbox image list
```

> 说明：Windows 进程容器无法直接运行 Linux 二进制，拉取的 rootfs 用于资源/工具链提取、调试与后续跨架构场景；`run --workdir` 可指向缓存中的 `rootfs` 作为工作目录。

## 隔离边界与限制

提供什么：

- **令牌隔离**：子进程持有 AppContainer 派生令牌，完整性级别为 **Low**（内核强制）；默认无任何 capability，文件/注册表/对象访问被限制在 Low IL 可写区域与 AppContainer 私有存储（`%LOCALAPPDATA%\Packages\<profile>` 风格的 AC 路径）。
- **资源限额**：每进程内存上限、CPU 硬性百分比上限（`JobObjectCpuRateControlInformation`，Windows 8+）、最大进程数。
- **生命周期收割**：`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 必开，wbox 退出/崩溃时整棵进程树被内核清理，无孤儿进程。
- **网络**：默认无 `INTERNET_CLIENT` capability → 无法发起网络连接；`--allow-network` 显式开启。

不提供什么（v1 非目标）：

- **文件系统 overlay / 注册表虚拟化**：`--workdir` 只是工作目录，不是只读视图；Low IL 天然阻止容器写大部分系统位置，但用户目录中 Low-IL 可写的位置仍可被写。真正的 FS 虚拟化需要 minifilter 驱动（Sandboxie 路线），见路线图。
- **网络命名空间**：Windows 无此原语；`--allow-network` 授予后没有流量级管控（防火墙规则需要管理员）。
- **GUI 桌面隔离**：AppContainer 自有 window station 边界只提供部分隔离。

## 与同类方案的区别

| 方案 | 隔离强度 | 需要 VT-x | 需要管理员 | 可选功能/驱动 |
|---|---|---|---|---|
| **wbox** | 进程级（令牌+Job） | 否 | 否 | 否 |
| Docker (Windows containers) | Silo/命名空间级 | 否（process 隔离）/ 是（Hyper-V 隔离） | 是 | Containers 功能 |
| Windows Sandbox | VM 级 | **是** | 是 | 可选功能 |
| Sandboxie(-plus) | 内核驱动 FS/注册表虚拟化 | 否 | 是 | 需装驱动 |

wbox 相当于"Sandboxie 的免驱动内核版 / Windows Sandbox 的无虚拟化版"：隔离更弱，但真正做到 portable、零依赖。

## 关键实现取舍：AppContainer 启动路径

SPEC 允许两条路径启动子进程：

1. `CreateProcessAsUserW` / `CreateProcessWithTokenW` + 手工派生的 AppContainer 令牌；
2. `CreateProcessW` + `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` attribute list。

**MVP 选择路径 2**，原因：

- 路径 1 需要调用方持有 `SeAssignPrimaryTokenPrivilege`（替换进程级令牌特权），普通用户默认没有 → 违背"默认不需要管理员"的定位；
- 路径 2 由内核在创建时基于当前令牌派生 AppContainer 子令牌，普通用户即可调用；
- 代价：attribute list 没有显式指定完整性级别的参数——但 AppContainer 派生令牌的 IL **恒为 Low**（内核强制），正好满足需求。

子进程以 `CREATE_SUSPENDED` 创建，**先入 Job 再 `ResumeThread`**，消除"进程在入 Job 前执行代码"的逃逸窗口。若 `AssignProcessToJobObject` 失败，wbox 会先 `TerminateProcess` 清理挂起的子进程再报错，避免留下 Job 收割不到的孤儿进程。

其他实现细节：

- **profile 复用**：`CreateAppContainerProfile` 返回 `ERROR_ALREADY_EXISTS`（profile 已存在）时视为成功，回退用 `DeriveAppContainerSidFromAppContainerName` 取 SID——同名容器可重复运行。
- **工作目录**：`--workdir` 只做存在性校验，不做 `canonicalize`（其产出的 `\\?\` 前缀路径不能作为 `CreateProcessW` 的 `lpCurrentDirectory`）。
- **参数转义**：命令行参数按 Windows CRT / `CommandLineToArgvW` 规则编码，
  包括引号前连续反斜杠、带空格且以反斜杠结尾的参数和空参数。

## 路线图

- **Silo 模式（可选高级模式）**：基于 Windows Server Container / host compute service 的命名空间级隔离（需要管理员 + Containers 功能，牺牲 portable）。
- **FS 虚拟化驱动**：minifilter + 注册表重定向，实现真正的 overlay（需要安装驱动，Sandboxie 路线）。
- **网络控制**：基于 WFP 的按容器出站规则（需要管理员）或用户态代理。
- `--detach` 后台模式、`wbox ps` / `wbox kill`（枚举/操作命名 Job 与 profile）、OCI 式"镜像目录清单"（layer 目录叠加挂载，依赖 FS 虚拟化）。

## 代码结构

分三层：**CLI**（`cli/`，纯逻辑跨平台可测）→ **后端抽象**（`backend/`，按目标分派 Native/Blink）→ **Win32 隔离原语**（`token/job/sandbox/acl`，仅 Windows 编译）。`oci/` 为 OCI 镜像拉取模块，纯 Rust 依赖（ureq+native-tls / tar / flate2 / sha2 / serde_json / base64），跨平台可编译。

```
src/
├── main.rs        # 模块声明 + 错误→退出码的唯一映射点
├── error.rs       # 带退出码语义的错误类型（1/2/3/4/5）
├── cli/
│   ├── mod.rs     # 子命令分发（run / image）与 USAGE
│   ├── args.rs    # 手写参数解析的共享原语（无 clap）
│   ├── run.rs     # run 子命令：参数 → RunSpec
│   └── image.rs   # image pull / list / show / rm
├── backend/
│   ├── mod.rs     # Backend trait、classify_target（本地 exe vs 镜像引用）、环境构造/命令校验单一出口
│   ├── native.rs  # NativeBackend：AppContainer + Job 直接跑 Windows 程序
│   ├── blink.rs   # BlinkBackend：拉起 wbox-linux.exe 跑 Linux ELF（BLINK_PREFIX 强制指向 rootfs）
│   └── env.rs     # 子进程环境构造：白名单 / 保留键剥离 / forced 覆盖 / 打印脱敏
├── oci/
│   ├── mod.rs     # 镜像引用解析（library/ 补全）、缓存目录布局、pull/list 编排
│   ├── registry.rs# Distribution v2 HTTP 客户端、匿名 Bearer token 流程、manifest/blob 拉取
│   ├── image.rs   # manifest list 平台选择、digest 链校验、gzip/纯 tar 解包、whiteout/硬链接、symlink 逃逸防护与降级复制
│   └── config.rs  # 镜像 config.json：Entrypoint/Cmd/Env/WorkingDir 的 docker 式合并
├── job.rs         # Job Object RAII 封装（KILL_ON_JOB_CLOSE / 内存 / CPU rate / 进程数）[仅 Windows]
├── token.rs       # AppContainer profile 创建/删除、capability SID、Low IL 说明 [仅 Windows]
├── sandbox.rs     # attribute-list 启动编排（挂起创建 → 入 Job → 恢复 → 等待 → 转发退出码）[仅 Windows]
├── acl.rs         # rootfs 授予 ALL APPLICATION PACKAGES 读/执行 ACE，使 AppContainer 子进程读得到镜像 [仅 Windows]
└── testenv.rs     # 测试脚手架：环境变量互斥 + Drop 自动还原（并行用例共享进程环境，见 docs/testing.md）[仅 cfg(test)]
```

所有 unsafe Win32 调用集中在 `token.rs` / `job.rs` / `sandbox.rs` / `acl.rs` 并附 `# Safety` 注释。

Linux 运行时 `wbox-linux.exe`（blink 的 Win32 移植）在 `vendor/blink/`，其移植层架构与已知限制见 `vendor/blink/WIN32-PORT.md`。
