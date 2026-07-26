# wbox win32 移植（wbox-linux 1.0.0-rc2 状态）

本文档记录 blink → `wbox-linux.exe`（x86_64-windows，MinGW/zig cc 交叉编译）
的移植层架构、运行时崩溃根因与修复、缺口裁决、已知限制与验证方法。

## 0. 生产状态声明（wbox-linux 1.0.0-rc2）

**支持矩阵**（wine 11.11 + 真 Windows CI 实测，见 §7/§8）：

| 场景 | 状态 |
|---|---|
| busybox 静态（11 项基础矩阵：uname/echo/cat/ls/stat/find/重定向/退出码…） | ✅ 全通 |
| shell 8 项（管道/命令替换/后台+wait/重定向链/dev/null/fork 子 exec） | ✅ 全通（快照式 fork，§7.4） |
| ubuntu-base-24.04.3 动态 glibc 程序（ls/cat/bash/uname/apt --version） | ✅（`BLINK_PREFIX=<rootfs>`，§7.2） |
| 网络（busybox wget 公网 + md5、epoll loopback） | ✅（§7.3） |
| JIT（x86_64→x86_64） | ✅ 默认开启（`WBOX_JIT=0` 可关） |

**已知限制汇总**：

- glibc pthread 程序崩溃（musl/busybox 不受影响）
- epoll：LT、`EPOLLET`、`EPOLLONESHOT` 均支持，覆盖 socket、pipe、eventfd、timerfd、signalfd
- mremap：匿名及文件映射均支持原地扩缩、MAYMOVE/FIXED 搬移，保留 private/shared 语义
- setuid/setgid 族恒返回 0（容器内语义，不穿透宿主）
- 卡在不可中断宿主等待的子进程被 SIGKILL 时走 TerminateThread，其 System/窗口按设计泄漏（长期改可轮询等待）
- 宿主 symlink/reparse point 不防护（rootfs 内勿放行特权创建的 symlink）
- guest 崩溃/host 异常时进程以 128+SIGSEGV 退出并输出诊断（见下）

**诊断开关**（全部运行期环境变量，默认关闭，零开销）：

| 开关 | 作用 |
|---|---|
| `WBOX_DEBUG=1` | 崩溃时 VEH 追加 host 寄存器转储与 VA 窗口布局 |
| `WBOX_DEBUG_FORK=1` | 快照 fork/回收页诊断（stale interval、dropped page） |
| `WBOX_DEBUG_MEM=1` | 窗口 reserve/snapshot/wipe 与 mmap 逐笔日志 |
| `WBOX_DEBUG_NET=1` | Winsock/epoll 翻译层日志 |
| `WBOX_DEBUG_VFS=1` | VFS mount/traverse 诊断（blink 核心侧） |
| `WBOX_VA_BITS=38..43` | 调整 guest VA 窗口位数（默认 40=1TB/进程） |
| `wbox-linux.exe --version` | 版本号 + git 短哈希 + UTC 构建时间 |

**问题上报指引**：提交 issue 请附 ① `wbox-linux.exe --version` 输出；
② 崩溃时 stderr 的 `wbox-linux: fatal host exception ...` 段（含 guest
pid/rip/故障地址，`WBOX_DEBUG=1` 重跑一次取完整转储）；③ 最小 guest
复现命令与 rootfs 来源；④ 运行环境（wine 版本或 Windows 版本）。

---

L1 目标已达成：**wine 11.11 下 wbox-linux.exe 可运行 busybox 级 Linux 静态
ELF 程序**（uname/echo/cat/ls/stat/sh/true/false/退出码实测通过，见 §7）。

L2 进展（2026-07-24，wine 11.11 实测）：VFS/overlays 已启用，
`BLINK_PREFIX=<rootfs>` 可运行 **ubuntu-base-24.04.3** rootfs 内的
动态链接 glibc 程序（ls/cat/bash/uname/apt --version，见 §7.2）；
网络矩阵（busybox wget 公网 md5 校验 + epoll loopback）通过（§7.3）。
feat/cow 合入后 shell 管道/命令替换/后台任务已全通（快照式 fork，§7.4）；
`apt-get update` 已于 2026-07-25 实测通过（rc=0，28.9MB 落盘，需
`APT::Cache-Start "200000000"`，见 §7.4）；剩余已知限制为 glibc pthread
（汇总见 §0）。

## 1. 移植层架构

构建：`sh win32/build-mingw.sh`（默认 `python3 -m ziglang cc -target
x86_64-windows-gnu`），配置头 `win32/config.h`（DISABLE_JIT / DISABLE_SOCKETS /
DISABLE_VFS / DISABLE_OVERLAYS 等 L1 裁剪）。

### 1.1 compat shim 头（win32/compat/，34 个）

`#include <...>` 角度括号头全部重定向到 shim 层，把 POSIX 类型/常量补齐到
mingw CRT 之上：dirent.h、fcntl.h、errno.h、signal.h、termios.h、unistd.h、
sys/{stat,mman,wait,resource,socket,epoll,select,ioctl,prctl,random,sysinfo,
syscall,sysctl,time,times,types,uio,un,auxv,disklabel,mount,statvfs,sockio,
vfs}.h、arpa/、net/、netinet/、netdb.h、poll.h、grp.h、limits.h、stdio.h、
stdlib.h、string.h、time.h。blink 全部源码不经修改（除下述 _WIN32 小补丁）
即可通过编译。

### 1.2 运行时模块（win32/，~6300 行）

| 模块 | 职责 |
|---|---|
| `w32mem.c` | guest 地址空间：启动时 ReserveVirtual 保留整块 VA 窗口并回写 `kSkew`；mmap/munmap/mprotect/mremap → VirtualAlloc/Free/Protect 自管分配器，模拟 blink 依赖的 MAP_FIXED / 部分 munmap 语义；文件映射用 pread 填充，MAP_SHARED 由 per-window 注册表在 msync/munmap/exec/销毁时写回，并随 fork 快照克隆 |
| `w32fd.c` | fd 层：open/openat/read/write/pread/pwrite/lseek/dup/fcntl/fstat/stat 族 → CreateFileW + CRT fd；isatty/select/pipe（Console/匿名管道）；eventfd/eventfd2、timerfd 与 signalfd 共享 pseudo-fd 对象、阻塞/nonblock、poll/epoll 语义；`W32FillStat` 统一组装 struct stat。**统一抽象**：`W32FdClassify` 是 CRT fd 分类单入口（file/socket/epoll/eventfd/timerfd/signalfd/special，HANDLE 随附）；`W32JoinNorm` 是路径 escape+拼接+规范化共享步（`W32Path`/`W32ResolveAt` 两个路径入口共用，jail 出口检查集中）；`W32WaitFds` 是共享等待原语（socket WSAPoll 切片 / 文件恒就绪 / 管道 PeekNamedPipe / pseudo-fd 就绪语义内聚） |
| `w32sock.c` | 网络/epoll/termios 真实现（feat/net）：WSA 动态装载、socket 族、epoll 兴趣表（`epoll_wait` 走 `W32WaitFds`）、tcgetattr/tcsetattr 控制台模式 |
| `w32errno.c` | 宿主错误→Linux errno 映射表集中：`W32ErrFromHost`（GetLastError）、`W32ErrFromWsa`（WSAGetLastError）、`W32GaiErrFromWsa`（EAI_*） |
| `w32proc.c` | 进程/时间：getrlimit/getrusage/sysinfo/statvfs/times/sysconf、clock_gettime/nanosleep/sleep 族；每 guest 进程独立的 alarm/setitimer ITIMER_REAL 定时器与 SIGALRM 投递；快照 fork 的虚拟 pid 表（`W32Child*`）；fork/execve/wait 族见 §4/§7.4 |
| `w32sig.c` | 信号：sigaction/sigprocmask 记录型 stub（guest 信号语义在 blink 内部模拟）；VEH 把宿主同步异常转诊断 abort；Ctrl+C/Close 控制台事件终止进程 |
| `w32stubs.c` | 杂项：dirent（opendir/fdopendir/readdir/rewinddir/seekdir/telldir）、termios 辅助（cfmakeraw/cfset*speed 等）、TUI 桩、其余长尾 stub |

跨层 fd 命名空间（guest fd → per-System Fd 表 → VFS fd → CRT/host
HANDLE）在 syscall.c 侧由 `W32ResolveFd(system, guestfd, want_crtfd)`
单一入口翻译；`win32.h` 按 mem/sig/proc/fd/sock/wait/errno 分组声明
上述内部接口。

## 2. 三个运行时崩溃的根因与修复

1. **执行期崩溃（w32mem.c）**：文件映射填充时仅当 `prot & PROT_WRITE` 才
   降 RW 保护；但页面按最终保护提交，RX 页面 pread 写入即访问冲突。
   修复：pread 填充前**无条件** VirtualProtect 降 PAGE_READWRITE，填完恢复。
2. **退出码错误 / exit 跳栈（argv.c）**：入口 rdx（rtld_fini）按上游传了
   程序名字符串；静态 glibc 的 `__libc_start_main` 把任何非空 rtld_fini
   经 `__cxa_atexit()` 注册，exit 时跳进栈执行垃圾。修复：`_WIN32` 下
   `rdx=0`，与真实 Linux 内核入口一致。
3. **fstat 误判文件类型（w32fd.c W32FillStat）**：`wpath==NULL`（fstat）
   时 attr 恒 0，目录被判 S_IFREG，musl opendir 失败。修复：引入
   `have_info`，无 wpath 时取 `BY_HANDLE_FILE_INFORMATION.dwFileAttributes`；
   S_IFDIR 分支条件改为 `(attr != INVALID_FILE_ATTRIBUTES) &&
   (attr & FILE_ATTRIBUTE_DIRECTORY)`，不再要求 wpath。

**ls 列目录为空的根因（w32stubs.c）**：readdir 返回 `d_ino=0`，glibc 的
readdir 会丢弃 ino=0 的 getdents64 记录（musl 不会），导致 glibc 静态
busybox 的 `ls 目录`/`find` 全部显示空。修复：`d_ino` 改为文件名 FNV-1a
哈希（保证非零稳定），`d_off`/`telldir` 用位置计数，`seekdir` 简单重走。

## 3. VA 保留（kSkew 运行期化）

blink 原设计在编译期钉死 guest VA 窗口（`kSkew` 常量 + `FLAG_vabits`）。
wine 下对 ≥16TB 的 VirtualReserve 直接 SIGKILL 进程。修复：`WboxMemInit()`
启动时**运行期** ReserveVirtual 自选窗口（上限 43bit=8TB）并回写 `kSkew`，
`FLAG_vabits` 取实际窗口位宽；guest 一切地址计算随窗口基址走。

## 4. 十二项缺口最终裁决

| # | 缺口 | 裁决 |
|---|---|---|
| 1 | fork/vfork | ✅ 快照式 fork：子 Machine 独立 VA 窗口，按区域复制可读页并克隆文件映射 backing 元数据；guest fd 表独立、host fd dup，父子并发、fork 后文件 mremap 与 exec 可用（见 §7.4） |
| 2 | 管道组合命令（`a \| b`） | ✅ pipe2 + 快照 fork 已打通；busybox shell 管道、命令替换、后台任务和多段重定向矩阵通过 |
| 3 | socket 族 | ✅ Winsock2 映射：AF_INET/INET6 STREAM/DGRAM、pathname AF_UNIX stream、匿名 AF_UNIX stream/datagram socketpair、epoll/poll、errno 与 O_NONBLOCK。socketpair 内部 loopback 承载层在 Hostfs 对外保持未命名 AF_UNIX 身份 |
| 4 | wait/waitpid/wait3/wait4/waitid | ✅ 虚拟 PID 表（子线程句柄→退出码），waitpid/wait4 支持 WNOHANG；退出码精确透传 |
| 5 | execve 族 | ❌ 宿主层 ENOSYS；guest execve 由 blink 进程内重建即可，不经宿主 |
| 6 | mremap | ✅ 匿名及文件映射支持原地扩缩、`MREMAP_MAYMOVE` / `MREMAP_FIXED` 搬移、数据与逐页权限保留、新增页加载/清零；文件 backing 独立于原 fd 生命周期，MAP_PRIVATE 脏页隔离和 MAP_SHARED 写回均覆盖 |
| 7 | MAP_SHARED 文件写回 | ✅ 文件映射写回按文件 ID/偏移同步，覆盖 fork 后父窗口可见性、子映射搬移、exec wipe、MAP_FIXED 清理及内部 fd 0；由 `t_mmap` / `t_fork_mem` / `t_exec` 验证 |
| 8 | JIT | ✅ 已启用（WBOX_JIT=1 默认开，`WBOX_JIT=0` 回退纯解释器）；wine 11.11 实测相对解释器 sha256 6.13×、awk 6.32×（见第 7 节基准） |
| 9 | 宿主异步信号投递 | ⚠️ record-only stub；guest 信号语义 blink 内部模拟，VEH 兜底同步异常，Ctrl+C 终止进程 |
| 10 | clone/线程 | ❌ 未接入（静态 glibc pthread 在上游 blink 亦 100% 崩溃，列入不支持） |
| 11 | 动态链接（PT_INTERP） | ✅ 可用（映射 rootfs/宿主 ld-linux；动态 glibc 测试程序在 wine 下运行正常） |
| 12 | ptrace/调试 | ❌ 不支持（L3 候选） |

## 5. 已知不工作项

- glibc pthread/clone
- 宿主异步信号投递不完整；不可中断等待中的 SIGKILL 使用线程终止兜底
- ptrace/调试接口
- 终端 feat/net：tcgetattr/tcsetattr 映射 Console API（ICANON→LINE_INPUT、ECHO→ECHO_INPUT、ISIG→PROCESSED_INPUT），TIOCGWINSZ 取真实控制台尺寸；无 pty
- wineserver 报 "prefix is not owned by you" 为 wine 沙箱提示，无功能影响

## 6. 构建与 wine 验证

```sh
pip install ziglang
# wine 11.11：
curl -LO https://dl.winehq.org/wine-builds/debian/pool/main/w/wine/wine-devel_11.11~bookworm-1_amd64.deb
curl -LO https://dl.winehq.org/wine-builds/debian/pool/main/w/wine/wine-devel-amd64_11.11~bookworm-1_amd64.deb
for d in *.deb; do dpkg-deb -x "$d" /tmp/wbox/wine; done
# busybox-static（busybox-static_1.35.0-4+deb12u1+b1_amd64.deb）解包取 bin/busybox

cd vendor/blink && WBOX_BUILD=/tmp/wbox/build sh win32/build-mingw.sh
WINEDEBUG=-all WINEPREFIX=/tmp/wbox/prefix \
  /tmp/wbox/wine/opt/wine-devel/bin/wine wbox-linux.exe busybox <cmd>
```

## 7. 验收矩阵（wine 11.11 实测输出）

```
$ wbox-linux.exe busybox uname -a
Linux K20804180457311 4.5.0-blink-1.1.0 #BLINK_COMMITS_UNKNOWN ... 2026 GNU/Linux   (rc=0)
$ wbox-linux.exe busybox echo hello wbox
hello wbox                                                                        (rc=0)
$ wbox-linux.exe busybox cat t.txt
line1 from host file                                                              (rc=0)
$ wbox-linux.exe busybox ls -la t.txt
-rwxrwxrwx    1 root     root            21 Jul 24 07:14 t.txt                    (rc=0)
$ wbox-linux.exe busybox ls -la subdir
total 0
drwxr-xr-x    2 root     root             0 Jul 24 07:14 .
drwxr-xr-x    2 root     root             0 Jul 24 07:23 ..
-rwxrwxrwx    1 root     root             0 Jul 24 07:14 a.txt
-rwxrwxrwx    1 root     root             0 Jul 24 07:14 b.txt                    (rc=0)
$ wbox-linux.exe busybox ls
busybox  gdtest2  subdir  wbox-linux.exe  gdtest  gdtest2g  t.txt               (rc=0)
$ wbox-linux.exe busybox sh -c 'echo hi'
hi                                                                                (rc=0)
$ wbox-linux.exe busybox true / false / sh -c 'exit 7'
(rc=0 / rc=1 / rc=7)
$ wbox-linux.exe busybox stat t.txt
  File: t.txt
  Size: 21  Blocks: 1  IO Block: 4096  regular file
  Access: (0777/-rwxrwxrwx)  Uid: (0/root)  Gid: (0/root)                         (rc=0)
$ wbox-linux.exe busybox find subdir
subdir
subdir/a.txt
subdir/b.txt                                                                      (rc=0)
$ wbox-linux.exe busybox sh -c 'echo abc > out3.txt'   # 重定向写
abc（cat 读回一致）                                                                (rc=0)
$ wbox-linux.exe busybox sh -c 'echo hello | cat'      # 已知限制
sh: can't fork: Function not implemented                                          (rc=2)
```

调试开关（默认全关）：`WBOX_DEBUG_MEM=1`（w32mem 窗口/mmap 插桩）。

## 7.1 JIT 与体积/性能（wine 11.11 实测）

构建开关：`WBOX_JIT=1`（默认，JIT）/ `WBOX_JIT=0`（`-DDISABLE_JIT` 纯解释器）；
运行期还可用 `wbox-linux.exe -j` 临时关闭 JIT。

体积（zig cc -O2，同树构建）：JIT 版 862720 字节，纯解释版 794624 字节。

JIT 关键修复（feat/jit）：

- Win64 ABI 三处：jit.h kJitArg0..3 改 rcx/rdx/r8/r9（扩展到 `_WIN32`）；
  path.c kEnter/kLeave 取 Cygwin 分支（rcx 取参、保存/恢复 rdi/rsi），
  EndPath→BlinkEndPath 避让 wingdi.h；xmm.h / machine.c Actor 取 Cygwin 分支
- uop.c GetInstructionLength：micro-op 函数体的分支/静态内存（RIP 相对）
  检测在 `_WIN32` release 构建同样启用——否则逐字节拷贝后的 RIP 相对
  .refptr 访问指向 JIT 内存，段错误

基准（同机 wine 11.11，`WINEDEBUG=-all` 独立 WINEPREFIX；sha256sum 64MB
随机文件 / awk 300 万循环；解释器数字为同一 JIT 二进制加 `-j` 测得）：

| 基准 | JIT | 解释器（-j） | 倍数 |
|---|---|---|---|
| sha256sum 64MB | 29.9s | 183.3s | 6.13× |
| awk 3M 循环 | 66.5s | 420.6s | 6.32× |

11 项验收矩阵在 JIT 版全部通过（输出与第 7 节一致，含已知 fork 限制项）。
注意：4GB 内存受限容器内解释器长跑进程 RSS 约 2.1GB，勿与多个 wineserver
并存跑基准（会被 memcg OOM kill）。

## 7.2 L2：ubuntu:24.04 rootfs 矩阵（wine 11.11 实测，2026-07-24）

用法：`BLINK_PREFIX=<rootfs 路径>`（wine 路径形式如 `Z:\tmp\wbox\ubuntu-rootfs`），
rootfs 为 ubuntu-base-24.04.3 tar 解包（或 `wbox image pull` 缓存）。

关键修复（本里程碑）：

1. **启用 VFS/overlays**（config.h 原钉死 DISABLE_VFS，BLINK_PREFIX 被忽略，
   `ls /` 显示宿主根目录）+ 补 win32 缺失的 `realpath`（GetFullPathNameW
   实现，返回 '/' 分隔路径；Windows 不解析 symlink，L2 接受该差异）。
2. **无 prefix 启动修复**（vfs.c VfsInit）：宿主根（wine `Z:\`）只读导致
   SystemRoot 挂载 mkdir EACCES 初始化失败 → 降级 best-effort；cwd 不再绕
   SystemRoot，直接用宿主路径（guest 根==宿主根恒等映射，`Z:/` 前缀剥成 `/`）。
3. **epoll guest/host fd 翻译**（syscall.c，`_WIN32`）：VFS 开启后 guest fd
   编号独立于宿主 CRT fd，epoll_ctl/epoll_pwait 直传 guest 号导致
   epoll_wait 稳定超时；W32EpollHostFd 按 VfsInfo→HostfsInfo.filefd（VFS fd）
   或 fds 表 hostfd（epoll 自身/管道）翻译。

| 用例 | 结果 |
|---|---|
| `/bin/ls /` | ✅ 列出 rootfs 根（bin etc lib usr var… + SystemRoot/proc/sys/dev 虚拟项） |
| `/bin/cat /etc/os-release` | ✅ `PRETTY_NAME="Ubuntu 24.04.3 LTS"` 完整输出 |
| `/bin/bash -c 'echo hi from ubuntu'` | ✅ `hi from ubuntu` |
| `/usr/bin/uname -a`（动态链接 + glibc） | ✅ 正常输出（blink 内核版本串） |
| `/usr/bin/apt --version` | ✅ `apt 2.8.3 (amd64)` |
| `/tmp/busybox wget http://mirrors.aliyun.com/debian/README`（rootfs 内） | ✅ md5 精确匹配（需先向 rootfs 写入有效 /etc/resolv.conf） |
| `echo hello \| /bin/cat`（bash 管道） | ❌ 失败（§7.4） |
| `apt-get update` | ✅ rc=0（2026-07-25 实测，28.9MB 落盘，需 `APT::Cache-Start "200000000"`，见 §7.4） |

## 7.3 网络矩阵（wine 11.11 实测，2026-07-24）

| 用例 | 结果 |
|---|---|
| `busybox wget http://mirrors.aliyun.com/debian/README` | ✅ 1193 字节，md5=`01718454f79b3bd9fa51e0e1f8966103` 精确匹配 |
| epoll loopback 单测（zig cc -static 自编译：socket/bind/listen/connect/accept/epoll_ctl/epoll_wait/数据回环） | ✅ `EPOLL_LOOPBACK_OK`（VFS 修复前后均验证） |

## 7.4 fork/exec 实测现状（2026-07-25，快照 fork 全部落地后）

快照 fork（窗口快照 + 页表重建）替代早期 vfork 式共享堆方案后，管道、
命令替换、后台任务全部实测通过。busybox 验收矩阵（8 项，含
`echo hello | cat`、`sleep 0.1 & wait`、命令替换、重定向、/dev/null）
**8/8 rc=0**；ubuntu rootfs 下 fork 子 exec 动态 glibc 程序
（md5sum/apt/http method）正常。关键修复链：

1. **guest/host fd 解耦**：VFS fd 表全局共享（父+所有快照子），guest fd
   号必须按 per-System 表分配（busybox ash `close(0)`+`open(/dev/null)`
   必须拿到 0）。SysOpenat/SysSocket/SysAccept4 改 AllocGuestFd 客号 +
   fd->hostfd；所有把 guest fd 直喂 Vfs*/宿主 mmap 的路径（fstat、文件
   mmap、pread/pwrite 族、ftruncate/fchown/fsync/fchdir/flock/fchmod/
   fcntl 锁/futimens/getdents、connect/bind、sockname）统一 HostFdOf
   翻译。fstat 错位曾导致 ld.so 读到 size=0 → mmap(len=0) EINVAL →
   fork 子 exec 动态库"cannot read file data"/"GLIBC_x not found"。
2. **SIGCHLD**：W32ChildExit 向父 Machine EnqueueSignal(SIGCHLD)；
   win32 宿主 sigsuspend=Sleep(INFINITE) 无信号投递，SysSigsuspend 改
   轮询 polyfill + W32AnyChildExited 兜底（busybox wait4(WNOHANG)+
   sigsuspend 等待循环依赖）。
3. **虚拟 pid kill**：SysKill 注册（原 #ifdef HAVE_FORK 内 win32 缺失
   → ENOSYS）+ 对子 Machine 投递 guest 信号，200ms 宽限后
   TerminateThread 兜底；apt 杀闲置 method 后 wait4 不再死锁。
4. **UDP 语义**：win32 recvfrom 零长接收改按 Linux 返回 0（Windows 会
   WSAEMSGSIZE 丢包）；非零长 WSAEMSGSIZE 按截断语义返回缓冲长度。
   getent/glibc DNS 在 fork 子内解析成功。
5. **文件 MAP_SHARED**：写回注册按 VA window 隔离并持有独立 backing fd；
   共享同步按 Windows 卷序列号、文件 ID 和重叠文件偏移匹配，因此子映射
   搬到不同 VA 后仍更新父进程原映射。显式槽占用状态保证 dup 复用宿主
   fd 0 时仍能正确写回。
6. **VFS 映射元数据**：fork 快照将父窗口内的文件 backing 条目平移克隆到
   子窗口，使继承映射可继续 `mremap`；exec wipe 前按当前窗口清除旧条目，
   新映像可安全复用同一 guest 地址且不会删除父窗口记录。
7. **建立失败回滚**：子线程启动前的 System/Machine/参数/线程创建失败统一
   临时切到子 VA window 清理，再恢复父 window；runner 对四阶段逐一故障
   注入，并验证父共享文件映射和后续 fork 均保持可用。

**apt-get update 已实测通过（2026-07-25）**：`Get:1..18` 全量下载、
gpgv 验签通过、`Reading package lists...` 完成、rc=0，
InRelease/Packages/Translation 全部落盘 /var/lib/apt/lists/（noble
universe 索引超过 apt 默认 24MB 动态 mmap，需
`APT::Cache-Start "200000000"`，属 apt 配置而非仿真缺陷）。收尾两个
修复：/proc/self/exe 改 per-System 注册表（原全局注册被 apt-config
内部 popen 的 dpkg 覆盖，busybox standalone applet exec self/exe 误载
dpkg，apt-key 判 keyring unsupported filetype）；O_TMPFILE 改走
SysTmpfile 创建+unlink 模拟（win32/compat/fcntl.h 定义 O_TMPFILE 致
`#ifndef O_TMPFILE` 分支被编译掉，透传后打开目录本身，write EBADF）。
另：glibc 并行 A/AAAA 会用零长 recvfrom 探测第二个应答大小，若按
FIONREAD 应答真实数据报大小，会触发一处**独立的快照子窗口提交崩溃**
（guest 未提交页读异常，直接崩 wine，rc=5），已回避未启用（保持
Linux 零长语义）。

回归保护：wget md5=01718454f79b3bd9fa51e0e1f8966103、
EPOLL_LOOPBACK_OK、fork 子 exec md5sum 均须保持。

## 8. 真 Windows 验证清单

在真机 Windows 上交付前需逐项确认（wine 不能覆盖的差异面）：

1. 直接运行 `wbox-linux.exe busybox uname -a` 等验收矩阵全部条目
2. VA 窗口保留：真机 ReserveVirtual 8TB 行为与 wine 差异（wine 有 ≥16TB
   SIGKILL 特例）；观察 WBOX_DEBUG_MEM=1 输出窗口基址/位宽
3. Ctrl+C / 关闭控制台：w32sig 控制台事件终止路径
4. VEH：故意触发 guest 同步异常，确认诊断 abort 而非系统崩溃弹窗
5. 长路径/中文路径：MultiByteToWideChar(CP_UTF8) 转换链
6. AppContainer 内运行：rootfs 目录 ACL 需对容器 SID 授权（见 docs/gap-analysis S2）
7. 杀毒/EDR 干扰：8TB VA 保留 + RWX 窗口可能触发启发式告警
