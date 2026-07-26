# wbox-linux 已知失败清单（C 层回归套件基线）

基线来源：`tests/run-guest-tests.sh`（wine 模式，wbox-linux v1.0.0-rc1，wine 11.11）。
基线统计：440+ 断言，FAIL 99（分布在下表 14 个用例文件；t_stress 全过）。
**以下均为测试暴露的真实缺陷，按要求未修生产代码。** 每条附疑似归属层与复现方法。

复现方法（任一条目）：

```sh
# 全量
WINE=wine WBOX_LINUX=build/wbox-linux.exe bash tests/run-guest-tests.sh
# 单个（wine 模式）
mkdir -p /tmp/wg && cp tests/guest/bin/<t_xxx> /tmp/wg && cd /tmp/wg &&
  WINEDEBUG=-all wine /path/to/wbox-linux.exe ./<t_xxx>
```

## P0 安全（审计 C2 防退化项 —— 当前已退化！）

| # | 现象 | 期望 | 实际 | 疑似归属层 |
|---|------|------|------|-----------|
| S1 | `open("/bin/../..../etc/hostname")` | EACCES | **成功读到宿主文件**（中间 `..` 组件逃逸） | VFS 路径归一化（win32 移植） |
| S2 | `open("/mnt/agents/...")`、`open("/etc/hostname")` 绝对路径 | EACCES/ENOENT | **成功读到宿主文件**（绝对路径越 rootfs；runner 默认无 BLINK_PREFIX 环境） | VFS 根限定 |
| S3 | `openat(dirfd, "../..")`（dirfd 位于 rootfs 内子目录） | EACCES | **成功**（dirfd 锚定可 `..` 逃逸） | VFS openat 实现 |
| S4 | 子目录中 `open("../../etc/hostname")` | EACCES | **wbox-linux 进程崩溃**（NULL deref，wine page fault @ wbox-linux+0x5be2b） | VFS 相对路径解析（隔离用例 t_sec_path_relesc，rc=5） |

## P1 内存语义

| # | 现象 | 期望 | 实际 | 疑似归属层 |
|---|------|------|------|-----------|
| M1 | `brk()` 增长（+64KiB/+8KiB） | 成功 | **ENOMEM**（任何增长都失败；sbrk 增量/负增量全链失败） | blink brk 堆管理（win32 提交粒度？） |
| M2 | `munmap(未映射地址)` | -1/EINVAL | **返回 0**（静默成功） | blink munmap 校验 |
| M3 | `mmap(MAP_SHARED)` 文件映射写入后 `pread` 文件 | 读到写入值 | **写未落盘**（shared 映射写不持久） | VFS/mmap 文件回写 |
| M4 | `mmap(MAP_ANONYMOUS, fd=有效fd)` | -1/EBADF | **成功**（旗标组合未校验） | blink mmap 参数校验 |
| M5 | fork 后 `MAP_SHARED\|MAP_ANONYMOUS` 页子写父读 | 父见子写（8） | **父读旧值（7）**——快照 fork 把 shared 页也复制了 | 快照 fork 共享页处理 |
| M6 | 写已保留未提交的 guest VA（brk 失败区域） | guest SIGSEGV | **模拟器自身崩溃**（wine 调试器接管） | blink 信号/缺页处理（由 M1 触发） |

## P1 fd/IO

| # | 现象 | 期望 | 实际 | 疑似归属层 |
|---|------|------|------|-----------|
| F1 | `open(O_CREAT, 0604)` 后 `fstat` 权限位 | 0604 | **0666**（mode 参数被忽略） | VFS open |
| F2 | `open(O_APPEND)` + `lseek(0)` + `write` | 强制追加到 EOF | **写到偏移 0**（O_APPEND 未生效；pwrite 同病） | VFS 写路径 |
| F3 | `pwrite` 到 >4GiB 偏移后 `fstat` | st_size = off+len | **尺寸不符**；空洞 `pread` 返回 -1；`pread` 后文件位置被移动 | VFS 大文件/off_t 64 位 |
| F4 | `pread(pipe)` | -1/ESPIPE | **返回数据（当 read 用）**；**空管道 pread 直接挂死模拟器**（无任何返回，120s 超时） | fd 层 pread 分派 |
| F5 | `unlink(仍打开的文件)` | 删除成功、名字消失（POSIX） | **返回 0 但文件仍在**（Windows 不能删打开文件，错误被吞） | VFS unlink |
| F6 | `unlink(目录)` | EISDIR | EACCES（次要，errno 精度） | VFS unlink |
| F7 | UTF-8 文件名创建（é你好） | 成功 | **ENOENT**（创建即失败，编码转换问题） | VFS 文件名 UTF-8↔UTF-16 |
| F8 | 特殊字符文件名（空格/'/"/()/[]） | 成功 | **EINVAL**（合法字符被拒绝） | VFS 文件名校验 |

## P1 进程

| # | 现象 | 期望 | 实际 | 疑似归属层 |
|---|------|------|------|-----------|
| P1 | `kill(pid,SIGTERM/SIGKILL)` + `waitpid` | WIFSIGNALED + WTERMSIG | **信号语义不成立**（子未按信号死亡/wait 状态错） | 进程/信号层 |
| P2 | `readlink("/proc/self/exe")` | 自身路径 | **失败**（rc 合并区曾修 per-System self/exe，回归套件环境下仍失败） | /proc 模拟 |

## P1 网络

| # | 现象 | 期望 | 实际 | 疑似归属层 |
|---|------|------|------|-----------|
| N1 | `socket(AF_UNIX, …)` / `socketpair(AF_UNIX)` | 成功 | **ENOSYS**（AF_UNIX 整体缺失） | 网络层 |
| N2 | `epoll_ctl(ADD)` 于 pipe / TCP 连接 socket | 成功 | **EBADF**（epoll 对管道与已连接 socket 不可用；LT/ONESHOT/MOD/DEL/RDHUP 全部连带失败） | epoll 层 fd 注册 |
| N3 | `socket(9999,…)` 非法 family | EAFNOSUPPORT(97) | ENOPROTOOPT(92)（errno 精度） | 网络层 |

## P2 errno 精度

| # | 现象 | 期望 | 实际 |
|---|------|------|------|
| E1 | `read(目录fd)` | EISDIR(21) | EINVAL(22) |
| E2 | `write(只读fd=stdin)` | EBADF(9) | EACCES(13) |

## 备注

- `symlink()` 创建全部 **EPERM**（含相对/绝对目标）——win32 宿主无特权创建符号链接属已知限制，
  连带 readlink/ELOOP/逃逸链用例退化；若最终判定为"不支持即合理"，可将 t_path/t_sec_* 中
  symlink 创建断言降级为 SKIP。
- t_exec 在 `/proc/self/exe` 缺失时回退 argv[0] 自 exec，exec 语义本体（argv/env/内存清洁）全过。
- t_stress（100 fork / 20 并发 / 1000 mmap 循环 / 1000 并发映射 / 64MiB 校验）**全部通过**。
