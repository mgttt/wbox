# wbox-linux 已知失败清单（C 层回归套件基线）

> **机器可读基线在 `tests/known-failures.txt`**（CI 门禁读它）。本文件是
> 人读的叙述与裁决理由。两者必须同步：从基线移除条目时，也要在此把对应
> 行改成"已修复"。判定语义（失败 ⊆ 基线放行 / 基线外新失败为回归 /
> 基线内用例变通过视为基线过期）见 `docs/testing.md` §一.2。

基线来源：`tests/run-guest-tests.sh`。真 Windows 机器基线为空；Wine 因
宿主 Winsock 不支持 AF_UNIX，登记 `t_net_sockopt @wine`。格式支持
`@native` / `@wine` 标注，以便精确登记环境专有缺陷
（无标注 = 两种模式都算）。

| 环境 | 当前基线 | 失败项 |
|---|---|---|
| 真 Windows（本机，native） | 20 个用例：**20 PASS / 0 FAIL / 0 SKIP** | 无 |
| wine（当前机器基线） | `t_net_sockopt @wine` | 新增 `t_eventfd`、`t_signal_timer`、`t_signalfd`、`t_timerfd` 待复核 |

真机断言级统计为 **858 条：pass=849 fail=0 skip=9**；skip 均为 symlink
EPERM 宿主限制降级与 t_exec 内的环境降级项。

**guest 套件自 2026-07-26 起为 CI 真门禁**（`guest-tests` job，取
build-wbox-linux 的 artifact + zig 交叉编译 + 基线判定），不再是 SKIP 空转。
全部历史缺陷（P0 路径安全 / P1 内存·fd·进程·网络 / errno 校准 / >4GiB / devfs A6 /
epoll 组 / brk / fork MAP_SHARED / MAP_SHARED 写回 / kill 语义 / self-exe）已修复并实测通过。
真机机器基线现已为空；Wine 仅保留 W4（`t_net_sockopt @wine`）。

复现方法（任一条目）：

```sh
# 全量
WINE=wine WBOX_LINUX=build/wbox-linux.exe bash tests/run-guest-tests.sh
# 单个（wine 模式）
mkdir -p /tmp/wg && cp tests/guest/bin/<t_xxx> /tmp/wg && cd /tmp/wg &&
  WINEDEBUG=-all wine /path/to/wbox-linux.exe ./<t_xxx>
```

## W1 ~~fork 依赖项挂死~~ → **已修复**（2026-07-26）

| # | 现象 | 根因 | 状态 |
|---|------|------|------|
| W1 | 一切依赖 `fork()` 的用例永久挂死（矩阵 B/C 两组 8 项，真 Windows 与 wine 9.0 均复现） | 快照对整个区间做一次 `memcpy`，而区间只验证过 `MEM_COMMIT`——**已提交 ≠ 可读**，撞上 `PAGE_NOACCESS`（guest PROT_NONE）页即触发访问违例；fork 期间 `m->canhalt` 为假，故障指令被反复重试 | ✅ 已修（按区域拷贝，跳过不可读页） |

**定位过程**（方法论上值得留档）：先后排除 8 条静态假设（guest close 延迟、
父子共享 fd、子退出不销毁 System、`VirtualQuery` 自旋、1TB 提交计费、
`IvInsert` 跨空洞合并、VEH 自死锁、锁泄漏），全部落空。转折点是**在 Linux
沙箱装 wine 做出本地复现**（见 docs/DEVELOPMENT.md §3.1）：迭代从 10 分钟
一轮变成几十秒，逐步加打点半小时收敛到指令级。

决定性的两行日志对比：

```
修复前： snapshot: iv[4] [00007EEF6B880000,00007EEF6B882000) 8192 bytes
         snapshot:   committed dst=00007DEF5B880000, memcpy..      ← 卡死
修复后： snapshot: skip unreadable [00007F1BE4C80000,00007F1BE4C81000) prot=0x1
         [w32fork] snapshot done → child thread enter → child thread blink
```

`prot=0x1` = `PAGE_NOACCESS`：8KB 区间里夹着一页 PROT_NONE，就这一页把整个
fork 钉死。**教训**：Windows 上"已提交"只说明有物理承诺，不代表当前保护属性
允许读；任何按区间批量读 guest 内存的地方都要先按区域看 `mbi.Protect`。

修复后矩阵：**PASS=14 → PASS=35 FAIL=3 SKIP=2**。当时剩余 3 个 FAIL 为
N1/E1/E2；E1/E2 后续已修复，见下方 P2。

## W2 ~~真 Windows 专有：sh 内 applet 解析失败~~ → **判定为测试用例缺陷，已修**

| # | 现象 | 判定 | 状态 |
|---|------|------|------|
| W2 | 矩阵 B4「重定向链 && cat」在真 Windows 上 `sh: cat: not found`（rc=127） | **不是产品缺陷**：用例自身依赖宿主 coreutils | 已修（B4 改用 `./busybox cat`） |

**破案过程（值得记住的一类陷阱）**：矩阵工作目录里只有 `busybox` / `t.txt` /
`subdir/` / `etc/resolv.conf`，**没有 `cat`，也没设 PATH**；而矩阵**不设
`BLINK_PREFIX`**，于是 guest 的 `/` 直通宿主 `/`。原用例写的是裸 `cat f`：

- **wine 模式**（Linux 宿主）：guest PATH 命中宿主真实的 `/usr/bin/cat` →
  用例"通过"。**这是假绿——它测的是宿主 coreutils，不是 wbox。**
- **真 Windows**：宿主没有 `/usr/bin/cat` → `not found`，如实报错。

即历史基线里 B4 的"8/8 全过"含一条虚假通过；真机 CI 反而把它暴露出来。
修法是把用例写死为 `./busybox cat f`（工作目录内的相对路径），两种模式下
测的都变成同一件事：重定向写入 + 由 wbox 执行的 guest 程序读回。

**教训**：矩阵不设 `BLINK_PREFIX` 时 guest 可见宿主文件系统，任何依赖
"命令能被 PATH 找到"的用例都可能在 Linux 宿主上假绿。新增用例一律用
工作目录内的相对路径显式指定被测二进制。

## W3 ~~真 Windows 专有：长路径超 MAX_PATH~~ → **已修复**（2026-07-26）

| # | 现象 | 根因 | 状态 |
|---|------|------|------|
| W3 | `t_path` 的 `path/long-nested` 在真 Windows 失败（wine 通过） | 宿主绝对路径超 `MAX_PATH`(260)，`mkdir`/`open` 返回 ENOENT | ✅ 已修复，已从基线移除 |

证据（CI run 52，真机）：

```
FAIL path/long-nested: t_path.c:154: write_file(p, "deep") => -1 errno=2 (No such file or directory)
guest-suite: PASS=12 FAIL=4 SKIP=0
```

用例构造 `t_p_L` + 20×`/1234567890` + `/leaf.txt` = **234 字符相对路径**，
加上宿主 jail 根前缀即越过 260。同一二进制在 wine 下 `PASS=13 FAIL=3`
（wine 不施加 MAX_PATH 限制）——**这正是 guest-tests 成为真门禁后抓到的
第一个真机缺陷**，此前它一直走 SKIP 路径空转。

根因：`win32/w32fd.c` 的路径层整体以 `MAX_PATH` 为界
（`wchar_t out[MAX_PATH]`、`wcslen(s) >= MAX_PATH` 直接拒绝），未使用
`\\?\` 扩展长度前缀。

修复将 Win32 路径缓冲统一扩到 32768 个宽字符；路径仍先经
`W32JoinNorm` 规范化并通过 `W32WithinRoot` jail 校验，随后才添加 `\\?\`
前缀交给 Win32 API。目录枚举缓冲也同步扩容，保证深目录下
`opendir`/`readdir` 可用。真机回归 `t_path` 为
**pass=50 fail=0 skip=3**，覆盖深层文件创建、读回和目录枚举。

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

## P1 网络 —— ✅ 已全部修复（2026-07-26）

| # | 现象 | 期望 | 实测现状 |
|---|------|------|-----------|
| N1 | `socket(AF_UNIX,…)` / `socketpair(AF_UNIX)` | 成功 | ✅ 普通 AF_UNIX stream socket 走现代 Winsock 原生实现；匿名 stream/datagram pair 由仅 loopback 可达的 Winsock 连接对承载，并在 Hostfs 元数据层保持未命名 AF_UNIX 身份。pathname bind/connect/accept、双向收发、epoll、NONBLOCK/CLOEXEC、getsockname/getpeername 全部通过 |
| N2 | `epoll_ctl(ADD)` pipe/TCP | 成功 | ✅ 已修复（fix/net-sem），t_net_epoll LT/ONESHOT/MOD/DEL/RDHUP 矩阵全过 |
| N3 | `socket(9999,…)` errno | EAFNOSUPPORT | ✅ 已修复（xlat.c 校准），t_negative 全过该项 |

## P2 errno 精度 —— ✅ 已全部修复（2026-07-26）

| # | 现象 | 期望 | 修复与实测 |
|---|------|------|------|
| E1 | `read(目录fd)` | EISDIR(21) | ✅ `ReadFile` 前识别目录句柄并返回 EISDIR |
| E2 | `write(只读fd=stdin)` | EBADF(9) | ✅ Win32 fd 层跟踪真实访问模式，`F_GETFL` 不再一律谎报 O_RDWR |

真机回归：`t_negative` **pass=24 fail=0 skip=0**；完整 guest 套件未出现
基线外新失败，`t_negative` 已从机器基线移除。

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
