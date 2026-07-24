# wbox win32 移植（L1 里程碑）

本文档记录 blink → `wbox-linux.exe`（x86_64-windows，MinGW/zig cc 交叉编译）
的移植层架构、运行时崩溃根因与修复、缺口裁决、已知限制与验证方法。

L1 目标已达成：**wine 11.11 下 wbox-linux.exe 可运行 busybox 级 Linux 静态
ELF 程序**（uname/echo/cat/ls/stat/sh/true/false/退出码实测通过，见 §7）。

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

### 1.2 运行时模块（win32/，~2600 行）

| 模块 | 职责 |
|---|---|
| `w32mem.c` | guest 地址空间：启动时 ReserveVirtual 保留整块 VA 窗口并回写 `kSkew`；mmap/munmap/mprotect/mremap → VirtualAlloc/Free/Protect 自管分配器，模拟 blink 依赖的 MAP_FIXED / 部分 munmap 语义；文件映射用 pread 填充（MAP_PRIVATE 拷贝语义） |
| `w32fd.c` | fd 层：open/openat/read/write/pread/pwrite/lseek/dup/fcntl/fstat/stat 族 → CreateFileW + CRT fd；isatty/select/pipe（Console/匿名管道）；`W32FillStat` 统一组装 struct stat |
| `w32proc.c` | 进程/时间：getrlimit/getrusage/sysinfo/statvfs/times/sysconf、clock_gettime/nanosleep/sleep 族；fork/execve/wait 族 ENOSYS（见 §4） |
| `w32sig.c` | 信号：sigaction/sigprocmask 记录型 stub（guest 信号语义在 blink 内部模拟）；VEH 把宿主同步异常转诊断 abort；Ctrl+C/Close 控制台事件终止进程 |
| `w32stubs.c` | 杂项：dirent（opendir/fdopendir/readdir/rewinddir/seekdir/telldir）、termios 哑控制台（cooked/raw 切换）、socket 族 ENOSYS、其余长尾 stub |

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
| 1 | fork/vfork | ⚠️ vfork 式特判：fork/vfork 以阻塞语义进程内实现（子 Machine 共享 System 线程，父阻塞至子 exec/exit；exec 时子切独立 VA 窗口，fd 表 host-dup 隔离）；`sh -c ':'; sh fork+exec 外部命令`已可用；真 COW fork 不支持；命令替换/管道在 fork 前仍有未定位崩溃（调试中） |
| 2 | 管道组合命令（`a \| b`） | ❌ 因 fork=ENOSYS（`sh: can't fork: Function not implemented`）；匿名管道本身（pipe2→CreatePipe）已实现 |
| 3 | socket 族 | ✅ feat/net：Winsock2 映射（win32/w32sock.c，ws2_32 GetProcAddress 惰性解析；AF_INET/INET6 STREAM/DGRAM、errno 映射、fcntl O_NONBLOCK）；socketpair 仍 ENOSYS；已知 wine 怪癖：wine 进程无法 connect wbox 进程创建的 listener（宿主侧互通正常，独立 issue 跟进） |
| 4 | wait/waitpid/wait3/wait4/waitid | ✅ 虚拟 PID 表（子线程句柄→退出码），waitpid/wait4 支持 WNOHANG；退出码精确透传 |
| 5 | execve 族 | ❌ 宿主层 ENOSYS；guest execve 由 blink 进程内重建即可，不经宿主 |
| 6 | mremap | ⚠️ 仅缩小或直接失败（ENOMEM）；busybox 罕见使用，L1 接受 |
| 7 | MAP_SHARED 文件写回 | ⚠️ 未实现（L1 gap，代码内注释标注）；文件映射按 MAP_PRIVATE pread 拷贝 |
| 8 | JIT | ✅ 已启用（WBOX_JIT=1 默认开，`WBOX_JIT=0` 回退纯解释器）；wine 11.11 实测相对解释器 sha256 6.13×、awk 6.32×（见第 7 节基准） |
| 9 | 宿主异步信号投递 | ⚠️ record-only stub；guest 信号语义 blink 内部模拟，VEH 兜底同步异常，Ctrl+C 终止进程 |
| 10 | clone/线程 | ❌ 未接入（静态 glibc pthread 在上游 blink 亦 100% 崩溃，列入不支持） |
| 11 | 动态链接（PT_INTERP） | ✅ 可用（映射 rootfs/宿主 ld-linux；动态 glibc 测试程序在 wine 下运行正常） |
| 12 | ptrace/调试 | ❌ 不支持（L3 候选） |

## 5. 已知不工作项

- fork/vfork/clone（ENOSYS）→ shell 管道、后台任务、多进程 applet 不可用
- socket 族（ENOSYS）
- mremap 扩容（ENOMEM）
- MAP_SHARED 写回
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
