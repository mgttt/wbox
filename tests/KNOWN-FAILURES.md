# wbox-linux 已知失败清单（C 层回归套件基线）

基线来源：`tests/run-guest-tests.sh`（wine 模式，wbox-linux v1.0，wine 11.11）。
基线统计（v1.0 全绿基线，2026-07-26 实测）：**16 个用例文件：13 PASS / 3 FAIL；
断言 423 条：pass=419 fail=4 skip=10**（skip 均为 symlink EPERM 宿主限制降级与
t_exec/t_net_epoll 内个别环境降级项）。
全部历史缺陷（P0 路径安全 / P1 内存·fd·进程·网络 / errno 校准 / >4GiB / devfs A6 /
epoll 组 / brk / fork MAP_SHARED / MAP_SHARED 写回 / kill 语义 / self-exe）已修复并实测通过。
残留仅下表 2 类共 4 条断言失败。

复现方法（任一条目）：

```sh
# 全量
WINE=wine WBOX_LINUX=build/wbox-linux.exe bash tests/run-guest-tests.sh
# 单个（wine 模式）
mkdir -p /tmp/wg && cp tests/guest/bin/<t_xxx> /tmp/wg && cd /tmp/wg &&
  WINEDEBUG=-all wine /path/to/wbox-linux.exe ./<t_xxx>
```

## P0 安全（审计 C2 防退化项）—— ✅ 已全部修复并复测通过（fix/fs-sec）

| # | 现象 | 期望 | 实测现状 |
|---|------|------|-----------|
| S1 | `open("/bin/../..../etc/hostname")` | EACCES | ✅ t_sec_path 全过（jail-by-default；`..` 越根拒绝） |
| S2 | 绝对路径（/mnt/agents/...、/etc/hostname） | EACCES/ENOENT | ✅ t_sec_path_abshost 全过（绝对路径 jail 到 WBOX_ROOT 内解析） |
| S3 | `openat(dirfd, "../..")` | EACCES | ✅ t_sec_path 全过（VfsHandleDirfdName + W32ResolveAt WithinRoot 复查） |
| S4 | 子目录 `open("../../etc/hostname")` | EACCES | ✅ t_sec_path_relesc 全过（原 NULL deref 崩溃已加回卷护栏） |

## P1 内存语义 —— ✅ 已全部修复并复测通过（fix/mem-proc 系列）

| # | 现象 | 实测现状 |
|---|------|-----------|
| M1 | `brk()` 增长 ENOMEM | ✅ t_brk 全过 |
| M2 | `munmap(未映射)` 静默成功 | ✅ t_mmap 全过 |
| M3 | `MAP_SHARED` 文件映射写不落盘 | ✅ t_mmap 全过（写回已生效） |
| M4 | `MAP_ANONYMOUS`+有效 fd 未校验 | ✅ t_mmap 全过 |
| M5 | fork 后 shared 页子写父不可见 | ✅ t_fork_mem 全过 |
| M6 | 写未提交 guest VA 模拟器自崩 | ✅ t_brk/t_mmap 全过（随 M1 修复） |

## P1 fd/IO —— ✅ 已全部修复并复测通过（fix/fs-sec + fix/mem-proc-2）

| # | 现象 | 实测现状 |
|---|------|-----------|
| F1 | `O_CREAT 0604` 权限位 | ✅ t_fd_open 全过（进程内 mode 仿真表） |
| F2 | `O_APPEND` 未生效 | ✅ t_fd_rw 全过（write/pwrite 强制追加到 EOF） |
| F3 | >4GiB `pwrite`/`fstat`/`pread` | ✅ t_fd_rw 全过（全程 64 位偏移与尺寸） |
| F4 | `pread(pipe)` 当 read 用/空管道挂死 | ✅ t_fd_rw 全过（返 ESPIPE） |
| F5 | `unlink(打开中文件)` | ✅ t_fd_open 全过（重命名隐藏临时名再删） |
| F6 | `unlink(目录)` 非 EISDIR | ✅ t_fd_open 全过 |
| F7 | UTF-8 文件名 | ✅ t_path 全过（宿主名 %XXXX 纯 ASCII 转义） |
| F8 | 特殊字符文件名 | ✅ t_path 全过（同转义方案） |

## P1 进程 —— ✅ 已全部修复并复测通过（fix/mem-proc 系列）

| # | 现象 | 实测现状 |
|---|------|-----------|
| P1 | `kill`+`waitpid` 信号语义 | ✅ t_proc 全过（WIFSIGNALED/WTERMSIG 成立） |
| P2 | `readlink("/proc/self/exe")` | ✅ t_proc/t_exec 全过 |

## P1 网络 —— 1 项残留（won't fix，宿主限制）

| # | 现象 | 期望 | 实测现状 |
|---|------|------|-----------|
| N1 | `socket(AF_UNIX,…)` / `socketpair(AF_UNIX)` | 成功 | **ENOSYS(38)**——**仍成立**（v1.0 实测：t_net_epoll:55 socketpair ENOSYS、t_net_sockopt:61 socket(AF_UNIX,SOCK_STREAM) 失败）。win32 宿主无 AF_UNIX，返回码干净、语义明确，本期不实现 |
| N2 | `epoll_ctl(ADD)` pipe/TCP | 成功 | ✅ 已修复（fix/net-sem），t_net_epoll LT/ONESHOT/MOD/DEL/RDHUP 矩阵全过 |
| N3 | `socket(9999,…)` errno | EAFNOSUPPORT | ✅ 已修复（xlat.c 校准），t_negative 全过该项 |

## P2 errno 精度 —— 1 项残留（真 bug，低优先级）

| # | 现象 | 期望 | 实测现状（v1.0） |
|---|------|------|------|
| E1 | `read(目录fd)` | EISDIR(21) | **仍失败**：EINVAL(22)（t_negative.c:94 实测复现） |
| E2 | `write(只读fd=stdin)` | EBADF(9) | **仍失败**：EACCES(13)（t_negative.c:100 实测复现） |

裁决：E1/E2 为真实缺陷（errno 校准不完整），不影响功能正确性，列入后续版本。

## 备注

- `symlink()` 创建 **EPERM**——✅ 已判定为宿主限制并降级 SKIP（fix/fs-sec）：
  probe 实测 wine 11 的 CreateSymbolicLinkW 只生成无法跟随的 reparse 占位文件
  （真实文件名带 `?` 后缀），非 wbox 缺陷；t_path 三个 symlink 块与
  t_sec_path/t_sec_linkabs 的创建断言在 EPERM 时降级为 SKIP（其他 errno 仍 FAIL）。
  v1.0 实测 skip=10 与此一致。
- t_exec 在 `/proc/self/exe` 缺失时回退 argv[0] 自 exec，exec 语义本体全过。
- t_stress（100 fork / 20 并发 / 1000 mmap 循环 / 1000 并发映射 / 64MiB 校验）全部通过。
- 环境噪音备忘（非缺陷）：① 部分宿主文件系统（FUSE/portal 盘）上 zig 直写产物偶发
  零填充损坏，tests/guest/build.sh 已改为本地临时盘编译 + cp/cmp 校验；② wine 首次
  创建 prefix 时并发跑首个用例可能 rundll32 c0000135，重跑即过；③ busybox 解析器读
  guest /etc/resolv.conf，test-matrix.sh 已在工作目录生成 nameserver 夹具；
  ④ blink 每次启动向 stderr 打 "Initializing VFS" INFO 行（上游固有行为），
  test-matrix.sh 的 bb() 已过滤该行。
