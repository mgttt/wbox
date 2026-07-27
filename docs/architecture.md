# wbox 技术架构

本文只描述长期有效的实现边界。产品范围和当前进度见 `../PRD.md`。

## 1. 总体分层

```text
CLI (`src/cli`)
├── run 参数 -> RunSpec
└── image pull/list/show/rm
        |
目标分类与环境构造 (`src/backend`)
├── Windows NativeBackend
├── Windows BlinkBackend
├── Linux LinuxNativeBackend
└── Linux Wine 执行器
        |
平台与数据层
├── Win32: token/job/sandbox/acl
├── Linux: user/PID/mount/net namespace + cgroup/rlimit
├── OCI: registry/config/image/cache
└── wbox-linux: `vendor/blink`
```

CLI 只负责解析和分派；`RunSpec` 表达后端无关意图；后端负责把网络、限额、
工作目录、环境和命令翻译为宿主机制。不可将某个平台的句柄、路径或 fd 语义
泄漏到公共层。

## 2. Windows 后端

### 2.1 原生程序

启动顺序固定：

1. 创建或复用 AppContainer profile，得到 SID。
2. 创建 Job Object，设置 kill-on-close 及请求的资源限额。
3. 使用 `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` 挂起创建进程。
4. 将进程加入 Job；失败则终止挂起进程并回滚。
5. 恢复线程、等待退出并转发退出码。
6. 按 `--keep-profile` 决定是否删除 profile。

attribute-list 路径避免普通用户通常没有的
`SeAssignPrimaryTokenPrivilege`。AppContainer 派生 token 的完整性级别为 Low。
默认不授予 capability；`--allow-network` 添加 `INTERNET_CLIENT`。

Job Object 提供：

- `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`。
- 每进程内存上限。
- CPU hard cap。
- active process limit。

`--workdir` 是工作目录，不是文件系统根或 overlay。

### 2.2 Linux ELF/OCI

`BlinkBackend` 定位同目录的 `wbox-linux.exe` 或 `WBOX_LINUX` 指定文件，
将 OCI rootfs 作为 `BLINK_PREFIX`，再经相同 AppContainer/Job 启动链执行。
rootfs 在启动前通过 ACL 给 AppContainer 读取和执行权限。

`wbox-linux` 是 blink 的 Win32 移植。它维护两层 fd：

- 每个 guest `System` 的 Linux fd 编号。
- 进程共享的宿主 VFS/Winsock/Win32 backing。

快照 fork 后两者编号可能不同。syscall 必须先在当前 `System` 的 guest fd 表
解析，之后只使用 host/VFS fd；元数据查找仍使用原 guest fd。不得把未跟踪的
guest 数字直接回退为同号宿主 fd，也不得重复翻译已转换的 fd。

完整移植结构、构建参数和 syscall 支持见
`../vendor/blink/WIN32-PORT.md`。

## 3. Linux 后端

### 3.1 公共隔离层

Linux 后端使用 rootless user namespace 映射当前 uid/gid，并建立 PID 与
mount namespace。默认新建 network namespace，只启用 loopback；
`--allow-network` 不创建 netns，从而共享宿主网络。

生命周期使用 PID namespace 与父死亡信号组合，目标是 wbox 异常退出后不遗留
后代。宿主禁止 unprivileged user namespace 时必须给出可操作错误，不得裸跑。

### 3.2 两种文件系统模式

- 镜像模式：bind/mount 必要节点后 `pivot_root` 到 OCI rootfs，旧根不可见。
- 宿主程序模式：不换根，`--workdir` 仅改变当前目录。

这一区分是产品契约。不要为了复用镜像逻辑让宿主程序意外失去宿主文件系统。

### 3.3 资源限制

cgroup v2 是 memory、pids 和 CPU 百分比的首选实现。能否使用取决于当前
cgroup 的写权限、控制器下发和 no-internal-process 规则，不能仅凭
`cgroup.controllers` 文件存在来判断。

实机取证已证伪旧布局：

- cgroup 内有进程时启用 `subtree_control` 返回 `EBUSY`。
- 空 cgroup 先启用控制器后，即使用 root 塞入进程也返回 `EIO`。
- 空父级下发控制器给 leaf 时，leaf 中的 `memory.max`、`pids.max` 和
  `cpu.max` 均存在且可写。

因此受限 target 必须是 wbox 所在 supervisor leaf 的兄弟，而不能是其子节点。
当前代码尚未完成这一布局改造，cgroup v2 首选路径仍无产品级覆盖。

改造时必须一并修掉一个既有缺陷：`try_cgroup_plan` 只在失败路径
（`abandon()`）删除刚创建的 cgroup 目录，成功路径从不清理。即每成功执行一次
`wbox run` 就在宿主留下一个空的 `wbox-<pid>` 目录，而 cgroup v2 不会自动回收。
该泄漏至今未被观察到，只是因为成功路径在任何环境都没有执行过；布局改对之后
会立即显现。这是通读代码发现的，测试不会报——零覆盖的路径不能默认其正确。

取证还要区分权限与规则错误：迁移进程要求对源和目标的共同祖先有写权限，
`cgroup.procs` 的 `EACCES` 本身不能证明 no-internal-process 规则。
`scripts/probe-cgroup2.sh` 只有在确认进程实际迁移成功后才对布局下结论。

仅当语义等价时允许 rlimit 回退。例如累计 CPU 秒数不等价于 CPU 百分比；
特权进程的 `RLIMIT_NPROC` 也不能可靠限制进程数。不能满足时明确拒绝。

### 3.4 Wine

PE 只在宿主程序模式交给 Wine，并复用上述隔离层。查找顺序受 `WBOX_WINE`
覆盖；默认 prefix 位于 `~/.wbox/wineprefix`。镜像 rootfs 中的 PE 当前明确
拒绝，因为宿主 Wine 及其依赖不在 pivot 后的 guest 根内。
verbose 模式会同时打印 Wine 路径和 `wine --version` 首行；版本获取失败只显示
“版本未知”，不阻断执行。

## 4. OCI 数据链

```text
引用解析
-> registry manifest/index
-> 平台选择
-> digest 校验
-> config 与 layer 下载
-> 按顺序应用 tar/whiteout
-> 写入 rootfs + manifest/config/layers 元数据
-> 运行时合并 Entrypoint/Cmd/Env/WorkingDir
```

关键不变量：

- 所有按 digest 获取的数据在使用前校验 SHA-256。
- registry 凭证只发往同 host 或明确允许的认证端点。
- 解包路径逐段解析，绝对路径、`..` 和 symlink 越界均不得逃出 rootfs。
- opaque whiteout 在应用本层普通条目前处理。
- Windows 无创建 symlink 权限时可以复制目标内容，但必须承认其不是引用语义。
- 缓存键必须包含 registry，且路径段适配 Windows 文件名限制。

## 5. 环境和命令

环境优先级为：

```text
后端强制值 > 镜像 Env > 允许继承的宿主 Env
```

默认只继承最小白名单。即使指定 `--env-pass-all`，`WBOX_*` 与 `BLINK_*`
内部键也必须剥离，再由后端写入可信值。日志和 `image show` 只做显示脱敏，
不能修改实际传入 guest 的普通镜像变量。

Windows 命令行必须按 `CommandLineToArgvW`/CRT 规则编码，特别处理空参数、
引号前反斜杠和以反斜杠结尾的带空格参数。

## 6. 代码所有权

| 路径 | 职责 |
|---|---|
| `src/cli/` | 解析、帮助文本、子命令分派 |
| `src/backend/` | 目标分类、RunSpec、环境及宿主后端 |
| `src/oci/` | 引用、registry、config、layer 和缓存 |
| `src/token.rs` | AppContainer profile/capability |
| `src/job.rs` | Job Object |
| `src/sandbox.rs` | Windows 进程启动编排 |
| `src/acl.rs` | Windows rootfs ACL |
| `vendor/blink/win32/` | wbox-linux 的 Win32 适配 |
| `tests/guest/` | Linux guest 行为回归 |

共享行为应放在既有公共模块；平台 FFI 留在平台模块。不要为一个调用点引入新
抽象，也不要在文档中复制可直接从 CLI 或代码生成的信息。

## 7. 设计红线

- 不因宿主能力不足而静默降低隔离。
- 不将 guest fd、VFS fd、Win32 HANDLE 和 Winsock SOCKET 当作同一编号空间。
- 不用字符串拼接替代 OCI JSON、tar 或路径组件解析。
- 不在子进程已开始执行后才加入资源管理对象。
- 不把网络可达性当作核心逻辑正确性的唯一证明。
- 不在含进程的 cgroup 节点上启用控制器并把受限 target 建为其子节点。
- 不把历史测试数字写入本手册；状态只维护在 `../PRD.md`。
