# Changelog

本项目所有重要变更记录于此文件。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。
里程碑以功能线回溯标注（项目以 trunk 滚动开发，tag 自 v1.0-rc 起）。

## [Unreleased]

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
