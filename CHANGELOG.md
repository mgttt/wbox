# Changelog

本项目所有重要变更记录于此文件。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。
里程碑以功能线回溯标注（项目以 trunk 滚动开发，tag 自 v1.0-rc 起）。

## [未发布] —— 真机 CI 首次打通（2026-07-26）

**里程碑：`build-wbox-linux` 在真 Windows 上首次转绿。** 此前该 job 从仓库
有 CI 起从未成功过；rc2 的"终审通过"实为 wine 单一环境下的结论，真机链路
的六层缺陷此轮被逐层剥开并修复。

### Fixed

- **AF_UNIX / socketpair（N1）**：现代 Windows 的 pathname AF_UNIX stream
  socket 走原生 Winsock；匿名 stream/datagram pair 由 loopback 连接对承载，
  同时在 Hostfs 层保留未命名 AF_UNIX 身份。修复 Win32 `SysSocketpair`
  遗漏的 guest/host fd 映射，并覆盖 bind/connect/accept、双向收发、epoll、
  NONBLOCK/CLOEXEC 与 getsockname/getpeername。guest 基线由 2 项收紧到空。
- **Win32 扩展长度路径（W3）**：路径层缓冲扩到 32768 个宽字符，在完成
  规范化和 jail 边界校验后才添加 `\\?\` 前缀；目录枚举链同步扩容。
  真机 `t_path` 的深层文件创建、读回及 `opendir`/`readdir` 回归现为
  **50 pass / 0 fail / 3 skip**，已移除 `t_path @native` 基线。
- **Win32 fd errno 精度（E1/E2）**：fd 适配层记录 open/pipe/dup 后的真实
  访问模式，`F_GETFL` 不再把 stdin/只读文件一律报成 `O_RDWR`，因此
  `write(只读 fd)` 正确返回 `EBADF`；`read(目录 fd)` 在 `ReadFile` 前识别
  目录句柄并返回 `EISDIR`。真机 `t_negative` 由 2 条失败转为
  **24 pass / 0 fail**，已从 known-failures 基线移除。
- **fork 永久挂死（W1，真机与 wine 9.0 均复现）**：`WboxMemSnapshotWindow`
  对每个区间先做整段 `memcpy`，而区间只验证过 `MEM_COMMIT`——**已提交 ≠
  可读**，撞上 `PAGE_NOACCESS`（guest PROT_NONE）页即触发访问违例；fork 期间
  `m->canhalt` 为假，故障指令被反复重试 → 永久挂死。改为按区域拷贝并跳过
  不可读页（保护属性仍在拷贝后再套）。修复后矩阵 **PASS=14 → PASS=35**，
  全部 fork 依赖项转绿。
- **堆损坏**：`posix_memalign` 用 `_aligned_malloc` 分配却被 `free()` 释放
  （0xC0000374，被 msys2 归一成 rc=127，长期伪装成"退出码转发失效"）。
- **`CreateAppContainerProfile` E_INVALIDARG**：`pszDescription` 必填却传了
  NULL——隔离主路径在真 Windows 上从来没跑通过。
- **`wbox-linux.exe` 依赖 libwinpthread-1.dll**：脱离 msys2 环境即
  `STATUS_DLL_NOT_FOUND`，链接改 `-static`，产物才真正符合 portable 定位。
- **构建链**：`SSIZE_MAX` 缺失（mingw 不提供，zig/musl 头有，故纯 zig 构建
  看不到）、exec 族与 CRT 符号冲突（改 `W32Exec*` + compat 头重定向）、
  响应文件路径被转义吃掉反斜杠（`cygpath -m` + `printf`）、并行编译静默
  吞掉编译错误（逐 pid 收状态）。

### Changed

- CI：六个门禁 job 全部加 `timeout-minutes`（此前无上界，最坏空烧 6 小时）；
  矩阵加单项超时（挂死记 `rc=124` 而非拖死 job）；`guest-tests` 接为真门禁
  （取 artifact + 基线判定）。
- 版本号收敛到 `Cargo.toml` 单一来源（此前 Cargo.toml / 构建脚本 / CHANGELOG
  三处互不相同，而 issue 模板要求用户附 `--version` 输出）。

### Notes

- **guest C 套件正式成为 CI 门禁**（此前 `guest-tests` 一直走 SKIP 空转，
  从未真正执行过）。补齐三层前置后首次真跑，随即抓到一个 wine 掩盖的真机
  缺陷（W3：长路径超 `MAX_PATH`，真机 ENOENT / wine 通过）并已修复——
  门禁的价值在它上线的第一轮就兑现了。相应地，`known-failures.txt` 支持
  `@native` / `@wine` 模式标注：同一份基线要同时服务两种宿主环境，而
  环境专有缺陷仍可被精确登记。
  W3 修复后基线收紧到 14 PASS / 2 FAIL；N1 随后修复，真机当前为
  16 PASS / 0 FAIL，机器基线已为空。

- **测试基线的一次修正**：矩阵 B1/B4/B7/B8 原用裸 `cat`/`grep`/`md5sum`，
  在不设 `BLINK_PREFIX` 时 guest `/` 直通宿主 `/`，wine 下命中宿主 coreutils
  而"通过"——即历史基线含**假绿**。已改为 `./busybox <applet>`。
- 新增本地 wine 复现环境说明（docs/DEVELOPMENT.md §3.1）。W1 在没有本地
  复现前，8 条静态假设全部落空；装上 wine 后迭代从 10 分钟变成几十秒。

## [v1.0.0-rc2] —— 终审通过（2026-07-26）

合并 win32 层抽象重构（R1-R5）与 Rust 重构（错误统一 / backend 下沉 /
cli 拆分 / `image rm`）后的发布终审，全部验证项实测通过：

- **缺陷歼灭**：累计修复 50+ 项（P0 路径安全 4 项、P1 内存/fd/进程/网络、
  errno 校准、>4GiB、devfs A6、epoll 组、brk、fork MAP_SHARED、kill 语义、
  self-exe 等，详见 tests/KNOWN-FAILURES.md 台账）。
- **重构**：win32 侧 R1-R5（w32 抽象统一，STUB_RENAMES 改名 hack 全删，
  w32stubs.c 干净编译）+ Rust 侧六项（错误统一 / backend 下沉 / cli 拆分 /
  `wbox image rm` 新命令 + 18 新测）。
- **测试基线（wine 11.11 实测）**：
  - guest C 回归套件 433 断言：**pass=419 / fail=4 / skip=10**（4 fail 为
    KNOWN-FAILURES 基线固有项：N1 AF_UNIX ×2、E1/E2 errno 精度 ×2）；
  - Rust 单测 **141 passed / 0 failed**；`cargo check --target
    x86_64-pc-windows-msvc` 与 `cargo clippy --all-targets` 均 0 warning；
  - 验收矩阵 **PASS=36 / FAIL=3（同 KNOWN-FAILURES）/ SKIP=1（epoll 预编译缺失）**；
  - `apt-get update`（ubuntu-base 24.04 rootfs，aliyun noble 源）rc=0，
    索引全数落盘；busybox wget md5=01718454f79b3bd9fa51e0e1f8966103 精确匹配。

### Added
- `scripts/test-matrix.sh`：wbox-linux 验收矩阵产品化脚本——11 项基础矩阵 +
  8 项 shell 矩阵 + fork 矩阵 + wget md5 校验 + epoll loopback 单测（无预编译
  二进制时 SKIP 并提示）；wine / msys2（真 Windows）双模式自动检测，
  失败计数并以非零码退出。
- CI `build-wbox-linux` job 在 windows-latest runner（真 Windows）上以
  native 模式跑完整验收矩阵，矩阵日志上传 artifact。
- CI `smoke-windows` job 增加真机镜像拉取验证
  （`wbox image pull hello-world --registry docker.m.daocloud.io`，
  registry 不可达时记 warning 标黄不红）。
- release 资产新增 `SHA256SUMS.txt`（wbox.exe / wbox-linux.exe / portable zip）。

## [v1.0-rc] —— 生产加固（当前线）

### Added
- 快照式 fork 全量落地（窗口快照 + 页表重建，全量页拷贝 + 父子真并发 +
  exec 就地 wipe+reload），替代早期 vfork 式共享堆方案，消灭 COW 级限制：
  管道、命令替换、后台任务全部实测通过；busybox shell 验收矩阵 8/8 rc=0。
- guest/host fd 全面解耦（per-System guest fd 表 + HostFdOf 翻译），
  fork 子 exec 动态 glibc 程序（md5sum/apt/http method）可用。
- SIGCHLD 投递（W32ChildExit → 父 Machine EnqueueSignal）+ sigsuspend
  轮询 polyfill；虚拟 pid kill（guest 信号投递 + 200ms 宽限 +
  TerminateThread 兜底）。
- UDP 语义对齐 Linux（零长 recvfrom 返回 0；WSAEMSGSIZE 按截断语义），
  getent/glibc DNS 在 fork 子内解析成功。

### Fixed
- 快照 iv 表误拷父窗口地址（重大）、回收页校验、窗口释放去提交。
- System 销毁真关 host 描述符（管道 EOF 泄漏）。
- fork 子 exec 动态库加载（guest/host fd 混淆致 ld.so 读 size=0）。
- 快照子信号死亡拖垮宿主：TerminateSignal 走子退出路径。

### Known issues
- `apt-get update` 存在 apt/http method 启动竞态（600 URI Acquire 偶发
  不发出，rc=124），未解；glibc 并行 A/AAAA 的 FIONREAD 探测路径回避中。

## [L2] —— 网络 + fork + JIT

### Added
- Winsock2 socket 族映射（win32/w32sock.c，ws2_32 惰性解析；
  AF_INET/INET6 STREAM/DGRAM、errno 映射、O_NONBLOCK）+ epoll + termios。
- VFS/overlays 启用 + win32 realpath，`BLINK_PREFIX` rootfs 生效：
  ubuntu:24.04 rootfs 下 bash/uname/apt --version 实测通过。
- vfork 式 fork/exec + 虚拟 pid wait（初版）。
- JIT 启用（WBOX_JIT=1 默认开）：Win64 ABI 修复 + micro-op RIP 相对拷贝
  根因修复；实测加速 6.1–6.3×（sha256sum 64MB / awk 3M 循环）。
- `wbox run` 全链串联（BlinkBackend prepare + resolv.conf 注入 +
  verbose 执行计划打印）。
- 网络矩阵实测通过：busybox wget 公网下载 md5 精确匹配 +
  epoll loopback 单测（EPOLL_LOOPBACK_OK）。

### Fixed
- wine phantom RSS/OOM：guest VA 窗口 43→40 位（规避 wine ≥16TB 保留区
  提交怪癖，真 Windows 无此开销）。
- poll() 管道 PeekNamedPipe 失败回退 WaitForSingleObject（busybox nc）。
- epoll guest/host fd 翻译（VFS 开启后 epoll_wait 稳定超时根因）。

## [L1] —— blink Win32 移植

### Added
- blink 1.1.0（ISC）vendor 为 wbox-linux 运行时基座；Win32 移植层五模块
  （w32fd/w32mem/w32proc/w32sig/w32stubs）+ 34 个 compat shim 头 +
  MinGW-w64 构建脚本（vendor/blink/win32/build-mingw.sh）。
- wine 11.11 下 wbox-linux.exe 可运行 busybox 级 Linux 静态二进制，
  11 项验收矩阵全部通过（uname/echo/cat/ls/stat/find/重定向/退出码转发）。

### Fixed
- 入口 rdx=0（静态 glibc rtld_fini 注册导致 exit 跳栈）。
- w32mem pread 降 RW 保护（执行期崩溃根因）；kSkew 运行期化 +
  ReserveVirtual 保留窗口。
- 目录读取全链：fdopendir/OpendirW/rewinddir、getdents64、
  dirent d_ino 非零（glibc readdir 丢弃 ino=0 记录致 ls 列空）、
  fstat 目录判定。

## [v0.2] —— OCI 镜像

### Added
- OCI 镜像拉取（纯 Rust，registry HTTP/TLS、分层解包、whiteout 处理）。
- `wbox run` 消费 OCI 镜像：Backend 抽象（NativeBackend/BlinkBackend），
  config.json 的 Env/Cmd/Entrypoint/WorkingDir 消费 + rootfs ACL 授权
  （ALL APPLICATION PACKAGES 读权，AppContainer 双层隔离衔接）。
- `wbox image pull / list / show` 子命令。

## [v0.1] —— 进程容器 MVP

### Added
- wbox：portable Windows 进程容器——AppContainer（令牌隔离 + Low IL）+
  Job Object（内存/CPU 硬性百分比/进程数限额 + 生命周期收割）。
- 默认不需要管理员权限、VT-x 或任何 Windows 可选功能。
- `wbox run --memory/--cpu-pct/--max-procs/--name/--workdir/--keep-profile`
  运行本地 Windows 程序；子进程退出码原样转发（wbox 自身错误 1–5）。

[Unreleased]: https://github.com/wbox/wbox/compare/main...HEAD
