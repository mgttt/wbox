# DEVELOPMENT — 新会话冷启动圣经

> 本文是 wbox 项目的单点入口文档：克隆后（或新会话冷启动时）只读这一篇，
> 即可知道项目是什么、现状到哪、怎么构建、怎么验证、坑在哪。
> 文档地图见文末 §8。

## 0. 一句话定位

**wbox** = portable Windows 进程容器：把任意 Windows 原生进程关进
**AppContainer（低完整性令牌）+ Job Object（资源限额/进程树收割）**
构成的隔离单元；内置 **wbox-linux**（blink 的 Win32 移植，用户态 x86-64
Linux 指令仿真），可在容器内直接运行 Linux ELF / OCI 镜像 rootfs。

目标机：Windows 10/11 / Server，**无 VT-x（无 WSL2/Hyper-V）**、
**默认不需要管理员**、不需要启用任何可选功能。单 exe 免安装。

- 仓库：https://github.com/mgttt/wbox
- 克隆：`git clone https://github.com/mgttt/wbox.git`
- 当前里程碑：**v1.0.0-rc2（终审通过）**，trunk 滚动开发，tag 自 v1.0-rc 起。

## 1. 仓库结构（冷启动必知）

```
wbox/
├── Cargo.toml / Cargo.lock   # 单 crate（workspace 根即 crate 根），edition 2021
├── src/
│   ├── main.rs               # CLI 解析（手写，无 clap）与命令分发、退出码约定
│   ├── cli/                  # mod.rs(分发+USAGE) / args.rs / run.rs / image.rs
│   ├── job.rs                # Job Object RAII（KILL_ON_JOB_CLOSE/内存/CPU rate/进程数）[Windows]
│   ├── token.rs              # AppContainer profile、capability SID、Low IL [Windows]
│   ├── sandbox.rs            # attribute-list 启动编排（挂起创建→入Job→恢复→等待→转发退出码）[Windows]
│   ├── acl.rs                # rootfs 目录 ACL 下放（Low IL 可读）[Windows]
│   ├── error.rs              # 带退出码语义的错误类型（1参数/2profile/3job/4进程创建/5镜像）
│   ├── testenv.rs            # [cfg(test)] 环境变量互斥+自动还原（并行用例共享进程环境）
│   ├── backend/              # 运行目标分流：native.rs（Windows 原生）/ blink.rs（Linux ELF）/ env.rs（环境合并）
│   └── oci/                  # OCI Distribution v2 客户端：mod(引用解析/缓存) / registry(HTTP+Bearer) /
│                             #   image(digest校验/tar解包/whiteout/symlink防护) / config(Entrypoint×Cmd合并)
├── vendor/blink/             # blink（justine 的 x86-64 Linux 仿真器）的 Win32 移植 → wbox-linux.exe
│   ├── win32/build-mingw.sh  # MinGW/zig cc 构建脚本（MSVC 编不了 GNU 扩展，勿用 MSVC 试）
│   └── WIN32-PORT.md         # 移植层架构/支持矩阵/已知限制（§0 生产状态声明）
├── scripts/test-matrix.sh    # wbox-linux 验收矩阵（wine / msys2 双模式自动检测）
├── tests/                    # guest C 回归套件：run-guest-tests.sh + KNOWN-FAILURES.md（基线台账）
├── docs/testing.md           # 三层测试体系与发布门禁
├── docs-architecture.md      # 架构总览
└── .github/workflows/ci.yml  # 6 门禁 job + tag 发布 job（见 §5）
```

依赖红线（SPEC 继承）：不加 tokio、不加 clap；HTTP 用 ureq+native-tls
（Windows=schannel，纯 FFI，保证 Linux 沙箱 `cargo check --target
x86_64-pc-windows-msvc` 可过；rustls/ring 需要 MSVC lib.exe，禁用）；
压缩用 flate2 rust_backend（miniz_oxide，纯 Rust）。所有 unsafe Win32
调用集中在 `token.rs` / `job.rs` / `sandbox.rs` / `acl.rs` 并附 `# Safety`。

## 2. 构建

### 2.1 wbox.exe（Rust）

```powershell
rustup target add x86_64-pc-windows-msvc
cargo build --release        # → target/release/wbox.exe（单文件 portable）
```

release profile 已按最小体积配置（opt-level=z / lto / codegen-units=1 /
panic=abort / strip）。

### 2.2 Linux 交叉构建（无 MSVC 的沙箱）

```sh
pip install ziglang          # zig cc 作 mingw 链接器
rustup target add x86_64-pc-windows-gnu
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=$PWD/win32-link-zig.sh \
  cargo build --release --target x86_64-pc-windows-gnu
```

坑：rustc 透传的 `-nodefaultlibs` 会让 zig lld no_fallback 找不到
libmsvcrt.a；`win32-link-zig.sh` 用 bash 数组重建参数过滤该 flag
（循环里直接改写 `"$@"` 不生效，勿改回）。

### 2.3 wbox-linux.exe（vendor/blink，C11+GNU 扩展）

```sh
# 真 Windows + MSYS2：
pacman -Sy --needed mingw-w64-ucrt-x86_64-gcc mingw-w64-ucrt-x86_64-binutils
WBOX_CC=x86_64-w64-mingw32-gcc sh vendor/blink/win32/build-mingw.sh
# → vendor/blink/build-win32/wbox-linux.exe
# Linux 沙箱：脚本默认 python3 -m ziglang cc -target x86_64-windows-gnu
```

## 3. 验证三层（发布门禁的本地复现）

```bash
cargo test --locked                                          # ① Rust 单测（Linux 177；Windows 188，0 ignored）
rustup target add x86_64-pc-windows-msvc
cargo check --locked --target x86_64-pc-windows-msvc         # ② Win32 编译门禁（要求 0 warning）
scripts/test-matrix.sh vendor/blink/build-win32/wbox-linux.exe ./busybox   # ③ 真机矩阵
WBOX_MATRIX_NET_SKIP=1 scripts/test-matrix.sh ...            #    网络用例不可达时保护
WINE=wine WBOX_LINUX=build/wbox-linux.exe bash tests/run-guest-tests.sh    # guest C 套件（wine）
```

### 3.1 在 Linux 沙箱里跑 wbox-linux（wine，强烈建议装）

Linux 开发机上装个 wine 就能**本地跑 guest 侧的一切**——矩阵、guest C 套件、
fork 调试——不必每次盲等 10 分钟的 CI。W1（fork 永久挂死）正是靠它破案的：
在没有本地复现前，连着 8 条静态假设全部落空；装上 wine 后几十秒一轮迭代，
逐步打点半小时定位到指令级。

```sh
apt-get install -y --no-install-recommends wine64
# 注意：Ubuntu 的 wine64 装在 /usr/lib/wine/wine64，**不在 PATH 上**
export WINE=/usr/lib/wine/wine64
export WINEPREFIX=/tmp/wp WINEDEBUG=-all      # 静默 + 独立 prefix

# 单点跑
$WINE vendor/blink/build-win32/wbox-linux.exe ./busybox echo hi
# 完整矩阵（脚本按 OSTYPE 自动选 wine 模式，认 $WINE）
WBOX_MATRIX_NET_SKIP=1 bash scripts/test-matrix.sh \
  vendor/blink/build-win32/wbox-linux.exe ./busybox
```

**wine 与真 Windows 的差异必须记住**：wine 下通过 ≠ 真机通过，反之亦然。
已实证的两类偏差——

- **真机独有的缺陷**：堆损坏（`posix_memalign` 配 `free()`）、DLL 缺失
  （`libwinpthread-1.dll`，已用 `-static` 修）在 wine 下都不显形；
- **wine 独有的假绿**：矩阵不设 `BLINK_PREFIX` 时 guest 的 `/` 直通宿主 `/`，
  裸 `cat`/`grep` 会命中宿主 coreutils 而"通过"（见 KNOWN-FAILURES W2）。
  **新增用例一律用工作目录内的相对路径**（`./busybox <applet>`）。

调试 guest 侧问题时按需打开：`WBOX_DEBUG_FORK`（fork 各阶段 + 快照逐区间）、
`WBOX_DEBUG_MEM`（窗口/mmap）、`WBOX_DEBUG_NET`、`WBOX_DEBUG_VFS`。
注意 `[w32fork]` 行走 `WriteFile` 直写不带缓冲，而 `wbox mem:` 行走
`fprintf(stderr)`——输出重定向到文件时**必须带 fflush 才不会在挂死被 kill
时整段丢失**（源码里已补，改动诊断代码时别退回去）。

约定：网络用例 registry 不可达时 **SKIP 不 fail**；严格失败路径一律用
本地构造输入覆盖。测试临时目录用 `pid()+tag` 拼唯一名、末清理；
需 HOME 的用例走 TempHome 脚手架。

## 4. CLI 与运行模型

```
wbox run [OPTIONS] -- <CMD> [ARGS...]     # --name/--memory/--cpu-pct/--max-procs/
                                          # --allow-network/--workdir/--keep-profile/-V
wbox image pull <REF> [--registry H] [-V] # OCI rootfs → %USERPROFILE%\.wbox\images\...
wbox image list / image rm
```

- 启动路径（关键取舍）：用 `CreateProcessW` +
  `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` attribute list（普通用户可用），
  不用 `CreateProcessAsUserW`（需 SeAssignPrimaryToken 特权）。AC 派生令牌
  IL 恒 Low（内核强制）。子进程 `CREATE_SUSPENDED` 创建，**先入 Job 再
  ResumeThread**，无逃逸窗口；assign 失败先 TerminateProcess 清场。
- 运行 Linux 镜像链路：镜像解析 → rootfs 定位 → config 合并
  （Entrypoint/Cmd/Env，优先级 forced > 镜像 > 宿主）→ 注入 resolv.conf 与
  `BLINK_PREFIX` → AppContainer 内拉起 wbox-linux.exe。
- 退出码：子进程码原样转发；wbox 自身 1参数/2profile/3job/4进程创建/5镜像。

## 5. CI（.github/workflows/ci.yml）

触发：push main、tag `v*`、PR、nightly（cron）、手动。6 个门禁 job：

| Job | Runner | 作用 |
|---|---|---|
| `test-linux` | ubuntu | Rust 单测（当前测试主体） |
| `test-windows` | windows | 同一套 `cargo test`，但在 windows 上跑——`cfg(windows)` 模块（sandbox/acl/job/token）的测试只有这里才编译得到 |
| `check-windows-msvc` | ubuntu(+target) | Win32 专属代码编译期门禁 |
| `smoke-windows` | windows | build + `--help`/`image list`/AppContainer 内 cmd 冒烟 + 真机 `image pull`（registry 不可达标黄不红） |
| `build-wbox-linux` | windows+MSYS2 | MinGW 构建 wbox-linux.exe + 完整验收矩阵（**PR 只跑冒烟**，矩阵留给 main/tag/nightly） |
| `guest-tests` | windows | **真门禁**：`needs: build-wbox-linux`，取其 artifact 拿 exe + 经 msys2 装 zig，跑 `tests/run.sh`。判定走 `tests/known-failures.txt` 基线（失败 ⊆ 基线放行；基线外新失败 = 回归；基线内变通过 = 基线过期，均判 FAIL）。artifact 缺失仍 SKIP 不阻塞 |

tag `v*` 时 `release` job needs 全部 6 个门禁 job 全绿，打包
wbox.exe + wbox-linux.exe + SHA256SUMS.txt 发 GitHub Release。

## 6. 当前状态（v1.0.0-rc2 基线）

- guest C 套件真 Windows：1186 pass / 0 fail / 9 skip，20/20 用例文件通过，
  native 基线为空（Wine 单列 `t_net_sockopt @wine`）。
- Rust 单测：真 Windows 198 passed / 0 failed / **0 ignored**，其中 11 项
  直接覆盖 AppContainer、Job Object 与完整启动链。
- 真机矩阵：PASS=43 / FAIL=0 / SKIP=1（独立 epoll 预编译二进制缺失；
  同等且更完整的覆盖由 guest `t_net_epoll` 提供）。
- 能力：busybox 静态全通；ubuntu-24.04 rootfs 动态 glibc
  （ls/cat/bash/uname/apt）✅；shell 8 项 + fork 矩阵 ✅（快照式 fork）；
  wget 公网 md5 ✅；epoll/socket ✅；`apt-get update` rc=0（aliyun 源实测）。
- 残留限制（WIN32-PORT.md §0）：宿主异步信号投递不完整；
  glibc pthread/clone 和 ptrace 尚不支持；
  JIT 默认开启（`WBOX_JIT=0` 关闭）。

## 7. 已知坑 / 冷启动注意事项

1. **开发沙箱是 Linux**：无法跑 Windows 功能测试；Win32 代码靠
   `cargo check --target x86_64-pc-windows-msvc` 门禁，真机信号靠 CI。
2. **别用 MSVC 编 vendor/blink**：GNU 扩展必须 MinGW/zig cc。
3. **guest 套件的已知失败走基线**：`tests/known-failures.txt` 是机器可读
   基线（CI 门禁读它），`tests/KNOWN-FAILURES.md` 是人读的裁决理由，
   **两者必须同步**。修好某项后要从基线移除，否则 runner 会以"基线过期"
   判 FAIL——这是刻意设计，防止修好的东西被基线继续掩盖。
   CI 里矩阵步骤设 `WBOX_GUEST_SKIP=1`，guest 套件由专职 job 独家承担，
   不重复跑；本地 `test-matrix.sh` 不设该变量，F 组照常执行。
4. **能力口径**：以 CHANGELOG 未发布段、`tests/KNOWN-FAILURES.md` 和
   最近一次绑定 main 提交的 CI 结果为准。
5. 参数转义已按 CommandLineToArgvW 反斜杠规则实现并由系统解析器往返测试；
   `--workdir` 不 canonicalize（`\\?\` 前缀不能作 lpCurrentDirectory）。
6. symlink 降级：普通用户无 SeCreateSymbolicLinkPrivilege，解包时
   降级为目标复制（开发者模式/管理员可真实创建）。
7. 诊断开关（运行期零开销）：`WBOX_DEBUG` / `WBOX_DEBUG_FORK` /
   `WBOX_DEBUG_MEM` / `WBOX_DEBUG_NET` / `WBOX_DEBUG_VFS` / `WBOX_VA_BITS`。

## 8. 文档地图

| 文件 | 内容 |
|---|---|
| `README.md` | 用户向：定位/构建/用法/隔离边界/路线图 |
| `docs/DEVELOPMENT.md`（本文） | 冷启动入口 |
| `docs/testing.md` | 三层测试体系与发布门禁 |
| `docs-architecture.md` | 架构总览 |
| `CHANGELOG.md` | 版本线（rc2 终审基线在此） |
| `tests/known-failures.txt` | guest 套件已知失败**基线**（机器可读，CI 门禁读它） |
| `tests/KNOWN-FAILURES.md` | 上表条目的叙述与裁决理由 + 复现方法（须与基线同步） |
| `vendor/blink/WIN32-PORT.md` | wbox-linux 移植圣经（§0 生产状态/支持矩阵/诊断开关） |
