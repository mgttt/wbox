//! Linux rootless 隔离原语（`LinuxNativeBackend` 的 L1 实现）。
//!
//! 对应 `PRD.md` F5：user + mount + pid namespace、
//! `uid_map`/`gid_map`、`pivot_root` 到 rootfs、`/proc` 最小挂载。全部在
//! **非 root** 下可用（unprivileged user namespace），与 wbox「默认不需要
//! 管理员」的定位一致——这一点与 Windows 侧选 attribute-list 路径而非
//! `CreateProcessAsUserW` 是同一条原则。
//!
//! # 为什么这些步骤必须在 fork 之后、exec 之前
//!
//! `Command::pre_exec` 的闭包运行在 fork 出来的子进程里、exec 之前，正是
//! 唯一能"改完命名空间再执行 guest 程序"的窗口。代价是该闭包受
//! **async-signal-safety** 约束：不能分配内存、不能用可能加锁的 libstd 设施。
//! 因此本模块把所有字符串在 fork **之前**构造成 `CString`，闭包内只做裸
//! syscall。这也是本文件大量使用 `libc::` 而非 `std::fs` 的原因。
//!
//! # PID namespace 的双 fork
//!
//! `unshare(CLONE_NEWPID)` 只影响**之后创建的子进程**，调用者自己仍留在
//! 旧 namespace。若直接 exec，guest 程序不会成为新 namespace 的 PID 1。
//! 故 unshare 后再 fork 一次：中间进程等待并转发退出码，孙进程才是 PID 1
//! 并继续 exec。

use super::super::{verbose_kv, Prepared, RunSpec};
use super::LinuxMode;
use crate::error::{Result, WboxError};
use std::ffi::CString;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// 把路径转成 fork 前就备好的 `CString`（闭包内不再分配）。
fn cstr(s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| WboxError::spawn(format!("路径含 NUL 字节：{:?}", s)))
}

/// 写 `/proc/self/<name>`，只用裸 syscall（async-signal-safe）。
/// 返回 0 成功、-1 失败（errno 已由 libc 设置）。
///
/// # Safety
/// `path`/`data` 必须是有效的 NUL 结尾指针，且在调用期间保持有效。
unsafe fn write_proc_self(path: *const libc::c_char, data: &[u8]) -> libc::c_int {
    let fd = libc::open(path, libc::O_WRONLY);
    if fd < 0 {
        return -1;
    }
    let n = libc::write(fd, data.as_ptr() as *const libc::c_void, data.len());
    libc::close(fd);
    if n < 0 {
        -1
    } else {
        0
    }
}

/// fork 前备好的全部参数（闭包按值捕获，内部零分配）。
struct NsPlan {
    /// `/proc/self/setgroups` / `uid_map` / `gid_map`
    p_setgroups: CString,
    p_uid_map: CString,
    p_gid_map: CString,
    uid_map: Vec<u8>,
    gid_map: Vec<u8>,
    c_root: CString,
    c_empty: CString,
    /// `unshare(2)` 的标志位，fork 前算好（是否含 `CLONE_NEWNET` 取决于
    /// `--allow-network`）。
    unshare_flags: libc::c_int,
    /// 是否新建 network namespace（决定要不要把 `lo` 拉起来）。
    new_netns: bool,
    /// fork 前的 wbox 自身 pid。用于 `PR_SET_PDEATHSIG` 的竞态自查：
    /// 若 prctl 之前父进程就已经死了，信号永远不会来，只能靠比对 `getppid()`
    /// 发现并自杀。
    expect_ppid: libc::pid_t,
    /// 换根计划。`None` = 宿主程序模式（[`LinuxMode::Host`]）：不 `pivot_root`，
    /// 宿主文件系统照常可见，只做身份/进程/网络/限额隔离。
    new_root: Option<NewRoot>,
    /// 资源限额落地方式（cgroup v2 或 rlimit 兜底）
    limits: LimitPlan,
}

/// 镜像模式的换根参数（[`LinuxMode::Image`] 才有）。
struct NewRoot {
    /// rootfs 的宿主绝对路径
    rootfs: CString,
    /// rootfs 内用于暂存旧根的目录（pivot_root 的 put_old）
    put_old: CString,
    /// 容器内的 /proc 挂载点与 put_old 卸载路径（pivot_root 之后的视角）
    c_proc: CString,
    c_put_old_in_new: CString,
    c_proc_fstype: CString,
    /// 最小设备集的 (宿主源, rootfs 内目标) 绑定对。**必须在 pivot_root 之前
    /// 完成**：切根后旧根已被 detach，宿主的 /dev/* 再也取不到。
    /// user namespace 里 mknod 不被允许，故只能 bind 宿主已有的设备节点。
    dev_binds: Vec<(CString, CString)>,
}

/// 资源限额的落地方式。cgroup v2 是首选（语义与 Windows 侧 Job Object
/// 最接近：内存/CPU/进程数都是"整组"上限）；不可用时退化为 setrlimit，
/// 但**只有部分语义能对上**，差异见各字段注释。
enum LimitPlan {
    /// cgroup v2：已创建的 cgroup 目录内 `cgroup.procs` 的路径。
    Cgroup { procs: CString },
    /// 无 cgroup v2：用 setrlimit 兜底。
    /// - `as_bytes`：RLIMIT_AS ≈ `--memory`。**语义差异**：Job Object 与
    ///   cgroup 限的是实际占用（RSS/page charge），RLIMIT_AS 限的是虚拟地址
    ///   空间总量，对大量 reserve-but-not-commit 的程序会偏严。
    /// - `nproc`：RLIMIT_NPROC ≈ `--max-procs`。**语义差异**：它是
    ///   *按 uid 计* 的全局进程数，不是本容器的；rootless 下容器内 uid 被
    ///   映射成 0，实际约束的是该映射 uid 的总进程数，仍能挡住 fork 炸弹。
    Rlimit { as_bytes: Option<u64>, nproc: Option<u64> },
    /// 无限额需求：不做任何事。
    None,
}

/// 定位自己所在的 cgroup v2 目录。返回 `Some(目录)` 仅表示**存在** v2 统一
/// 层级，**不代表能用**——委派没开时那个目录里建不了子目录，控制器没在父级
/// `cgroup.subtree_control` 里启用时对应的 `*.max` 文件根本不存在。
/// "能不能用"由 [`try_cgroup_plan`] 实地一试来判定，不在这里猜。
///
/// 这个区分是被 CI 打脸打出来的：GitHub 的 ubuntu runner 是 cgroup v2、
/// 控制器齐全，但 runner 用户对自己的 cgroup 目录**没有写权限**（委派未开）。
/// 早先这里只查 `cgroup.controllers` 存在就返回 Some，于是 `build_limit_plan`
/// 认定"有 v2"却写不进去，把一个本可以退化到 rlimit 的情形变成了硬报错。
fn cgroup2_self_dir() -> Option<std::path::PathBuf> {
    let content = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let rel = content
        .lines()
        .find_map(|l| l.strip_prefix("0::"))?
        .trim_start_matches('/')
        .to_string();
    let dir = std::path::Path::new("/sys/fs/cgroup").join(&rel);
    // 必须确认 v2 特征文件存在，否则可能是 v1 的 tmpfs
    if !dir.join("cgroup.controllers").is_file() {
        return None;
    }
    Some(dir)
}

/// 容器最小设备集。选取依据：shell 重定向到 /dev/null 是最常见的刚需
/// （实测缺了它 busybox sh 直接报 "can't create /dev/null"），
/// zero/random/urandom 被大量程序用于初始化，tty 供交互式程序探测。
const MIN_DEVICES: &[&str] = &["null", "zero", "full", "random", "urandom", "tty"];

/// 在隔离环境中启动 guest 并等待退出。
///
/// `mode` 决定是否换根：[`LinuxMode::Image`] 进镜像 rootfs，
/// [`LinuxMode::Host`] 直接跑宿主程序（只做身份/进程/网络/限额隔离）。
pub(super) fn spawn_isolated(
    spec: &RunSpec,
    prepared: &Prepared,
    mode: LinuxMode,
) -> Result<u32> {
    let new_root = match mode {
        LinuxMode::Host => None,
        LinuxMode::Image => Some(build_new_root(prepared)?),
    };

    // 网络：默认**不给**（新建空 network namespace），与 Windows 侧不授
    // INTERNET_CLIENT 能力的默认一致。PRD F5 一致性要求同一条命令
    // 在两个宿主上默认隔离强度相同，故这里不能"Linux 上顺便继承宿主网络"。
    let new_netns = !spec.allow_network;
    let unshare_flags = unshare_flags(spec.allow_network);

    // rootless 映射：把宿主当前 uid/gid 映射成容器内的 root。
    // 写 gid_map 之前必须先写 setgroups=deny，否则内核拒绝（CVE-2014-8989 之后的加固）。
    let limits = build_limit_plan(spec)?;
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let plan = NsPlan {
        p_setgroups: cstr("/proc/self/setgroups")?,
        p_uid_map: cstr("/proc/self/uid_map")?,
        p_gid_map: cstr("/proc/self/gid_map")?,
        uid_map: format!("0 {} 1\n", uid).into_bytes(),
        gid_map: format!("0 {} 1\n", gid).into_bytes(),
        c_root: cstr("/")?,
        c_empty: cstr("")?,
        unshare_flags,
        new_netns,
        expect_ppid: unsafe { libc::getpid() },
        new_root,
        limits,
    };

    if spec.verbose {
        let ns_list = if new_netns {
            "user+mount+pid+net namespace（rootless）"
        } else {
            "user+mount+pid namespace（rootless，共享宿主网络）"
        };
        verbose_kv("隔离", ns_list);
        verbose_kv("uid 映射", format!("容器 0 ← 宿主 {}", uid));
        verbose_kv(
            "网络",
            if new_netns {
                "断开（新建空 network namespace，仅 loopback）"
            } else {
                "允许（--allow-network：继承宿主网络栈）"
            },
        );
        match mode {
            LinuxMode::Image => verbose_kv("新根", prepared.workdir.display()),
            LinuxMode::Host => verbose_kv("新根", "无（宿主程序模式，不换根）"),
        }
    }

    let mut cmd = Command::new(&prepared.cmd[0]);
    // 宿主程序模式下 workdir 是真正的工作目录（镜像模式里它是 rootfs，
    // 换根后 pre_exec 已经 chdir("/")，不能在这里设）。
    if mode == LinuxMode::Host {
        cmd.current_dir(&prepared.workdir);
    }
    cmd.args(&prepared.cmd[1..]);
    // 环境完全由 prepared.env 决定（策略见 backend::env），不继承宿主。
    cmd.env_clear();
    for (k, v) in &prepared.env {
        cmd.env(k, v);
    }

    // # Safety
    // 闭包在 fork 之后、exec 之前运行，必须 async-signal-safe：
    // 只调用裸 syscall，不分配内存（所有 CString 已在 fork 前构造），
    // 不触碰可能被其它线程持有的锁。
    unsafe {
        cmd.pre_exec(move || enter_namespaces(&plan));
    }

    let mut child = cmd.spawn().map_err(|e| {
        WboxError::spawn(format!(
            "启动容器进程 '{}' 失败：{}{}",
            prepared.cmd[0],
            e,
            spawn_failure_hint(&e)
        ))
    })?;
    let status = child
        .wait()
        .map_err(|e| WboxError::spawn(format!("等待容器进程失败：{}", e)))?;
    // 退出码语义与 Windows 侧一致：子进程码原样转发；被信号杀死时用 128+sig
    // （shell 惯例），避免"信号终止"被误报成正常退出。
    Ok(match status.code() {
        Some(c) => c as u32,
        None => {
            use std::os::unix::process::ExitStatusExt;
            128 + status.signal().unwrap_or(0) as u32
        }
    })
}

/// 组装 `unshare(2)` 的标志位。
///
/// user + mount + pid 恒有；network namespace **默认也建**，只有
/// `--allow-network` 才共享宿主网络栈——与 Windows 侧默认不授
/// `INTERNET_CLIENT` 能力对齐。单独成函数是为了可测：`spawn_isolated`
/// 会真的 fork，不便在单测里断言。
fn unshare_flags(allow_network: bool) -> libc::c_int {
    let mut f = libc::CLONE_NEWUSER | libc::CLONE_NEWNS | libc::CLONE_NEWPID;
    if !allow_network {
        f |= libc::CLONE_NEWNET;
    }
    f
}

/// 备好镜像模式的换根参数（在 fork **之前**做完所有分配与目录准备）。
fn build_new_root(prepared: &Prepared) -> Result<NewRoot> {
    let rootfs = prepared.workdir.to_string_lossy().into_owned();
    // put_old 必须位于新根之内（pivot_root 的硬性要求）。用固定名字，
    // 每次启动前确保存在；pivot_root 后立刻 umount 并删除。
    let put_old_host = prepared.workdir.join(".wbox_oldroot");
    std::fs::create_dir_all(&put_old_host).map_err(|e| {
        WboxError::spawn(format!(
            "创建 pivot_root 暂存目录 '{}' 失败：{}（rootfs 是否可写？）",
            put_old_host.display(),
            e
        ))
    })?;

    // 最小设备集：bind 挂载需要目标文件已存在，故在 fork 前先备好空文件。
    // rootfs 不可写时跳过该设备而不是整体失败——只读 rootfs 是合法用法。
    let dev_dir = prepared.workdir.join("dev");
    let _ = std::fs::create_dir_all(&dev_dir);
    let mut dev_binds = Vec::new();
    for d in MIN_DEVICES {
        let src = format!("/dev/{}", d);
        if !std::path::Path::new(&src).exists() {
            continue;
        }
        let dst = dev_dir.join(d);
        if !dst.exists() && std::fs::write(&dst, b"").is_err() {
            continue; // rootfs 只读或无权限：跳过这个设备
        }
        if let (Ok(a), Ok(b)) = (cstr(&src), cstr(&dst.to_string_lossy())) {
            dev_binds.push((a, b));
        }
    }

    Ok(NewRoot {
        rootfs: cstr(&rootfs)?,
        put_old: cstr(&put_old_host.to_string_lossy())?,
        c_proc: cstr("/proc")?,
        c_put_old_in_new: cstr("/.wbox_oldroot")?,
        c_proc_fstype: cstr("proc")?,
        dev_binds,
    })
}

/// `SIOCSIFFLAGS` 用的 `ifreq` 前缀。自己声明而不用 `libc::ifreq`：后者的
/// `ifr_ifru` 是 union，跨 libc 版本字段名不稳定，而这里只需要 flags 这一支。
/// 大小必须与内核的 `struct ifreq` 一致（64 位下 40 字节）。
#[repr(C)]
struct IfReqFlags {
    name: [libc::c_char; libc::IF_NAMESIZE],
    flags: libc::c_short,
    _pad: [u8; 22],
}

/// 把新 network namespace 里的 `lo` 拉起来。
///
/// 新建的 netns 只有一个 **DOWN** 的 loopback，此时连 `127.0.0.1` 都不通。
/// 大量程序（本地 socket 通信、语言运行时自检）会因此莫名失败，而这与
/// "不给外网"的意图无关，故与 podman/docker 的默认一致：断外网但留 loopback。
///
/// 失败不致命——最坏情况是 guest 自己在用 loopback 时报错，比整个容器起不来好。
///
/// # Safety
/// 仅在 fork 后、已进入新 netns 的子进程中调用；内部只做裸 syscall。
unsafe fn bring_up_loopback() {
    const SIOCSIFFLAGS: libc::c_ulong = 0x8914;
    let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
    if fd < 0 {
        return;
    }
    let mut ifr: IfReqFlags = std::mem::zeroed();
    ifr.name[0] = b'l' as libc::c_char;
    ifr.name[1] = b'o' as libc::c_char;
    ifr.flags = (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
    libc::ioctl(fd, SIOCSIFFLAGS, &mut ifr as *mut IfReqFlags);
    libc::close(fd);
}

/// 宿主对 unprivileged user namespace 的三种常见封禁方式。
/// 分开成纯函数是为了可测——真去关掉宿主的 userns 没法在单测里做。
///
/// 参数就是三个 sysctl 的原始内容（`None` = 该文件不存在）。
fn userns_hint_from(
    apparmor_restrict: Option<&str>,
    unpriv_clone: Option<&str>,
    max_user_ns: Option<&str>,
) -> Option<&'static str> {
    if apparmor_restrict.map(str::trim) == Some("1") {
        // Ubuntu 24.04 起的默认值。GitHub 的 ubuntu runner 就是这个配置，
        // 本项目的 CI 门禁曾因此静默零覆盖，说明这条路很多人会踩。
        return Some(
            "\n提示：本宿主由 AppArmor 禁用了 unprivileged user namespace\
             （kernel.apparmor_restrict_unprivileged_userns=1，Ubuntu 24.04+ 的默认值）。\
             wbox 的隔离以它为前置。解法：\
             `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`（临时），\
             或为 wbox 装一份允许 userns 的 AppArmor profile，或以 root 运行",
        );
    }
    if unpriv_clone.map(str::trim) == Some("0") {
        return Some(
            "\n提示：本宿主禁用了 unprivileged user namespace\
             （kernel.unprivileged_userns_clone=0，部分 Debian/RHEL 衍生版的默认值）。\
             解法：`sudo sysctl -w kernel.unprivileged_userns_clone=1`，或以 root 运行",
        );
    }
    if max_user_ns.map(str::trim) == Some("0") {
        return Some(
            "\n提示：本宿主把 user namespace 配额设成了 0\
             （user.max_user_namespaces=0）。\
             解法：`sudo sysctl -w user.max_user_namespaces=<正整数>`",
        );
    }
    None
}

/// 给 spawn 失败补一条可操作的提示。
///
/// 起因：在 Ubuntu 24.04（含 GitHub ubuntu runner）上，wbox 只会打印
/// "启动容器进程 '/bin/echo' 失败：Operation not permitted"——用户完全看不出
/// 是宿主关了 unprivileged user namespace，更不知道怎么开。权限类错误才去查
/// 那几个 sysctl，其它错误（命令不存在等）原样返回，不添乱。
fn spawn_failure_hint(e: &std::io::Error) -> String {
    use std::io::ErrorKind;
    if !matches!(e.kind(), ErrorKind::PermissionDenied | ErrorKind::InvalidInput) {
        return String::new();
    }
    let read = |p: &str| std::fs::read_to_string(p).ok();
    let a = read("/proc/sys/kernel/apparmor_restrict_unprivileged_userns");
    let c = read("/proc/sys/kernel/unprivileged_userns_clone");
    let m = read("/proc/sys/user/max_user_namespaces");
    userns_hint_from(a.as_deref(), c.as_deref(), m.as_deref())
        .unwrap_or("")
        .to_string()
}

/// 让本进程在父进程死亡时被 `SIGKILL`（L3 生命周期绑定）。
/// 返回 0 成功、-1 失败（errno 已设置）。
///
/// 这是 Windows 侧 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 在 Linux 上的对应物，
/// 同样**必开**：wbox 被 SIGKILL 后不能留下还在跑的 guest 进程。
///
/// `expect_ppid` 给 `Some(pid)` 时会额外关掉一个真实竞态：若父进程在 `prctl`
/// **之前**就已经死了，PDEATHSIG 永远不会送达，本进程会变成没人管的孤儿。
/// 故设完立刻比对 `getppid()`，发现父进程已换（通常是被重新 parent 到
/// init/subreaper）就直接自杀。
///
/// 给 `None` 是留给**新 PID namespace 里的 PID 1** 的：它的父进程在祖先
/// namespace 里，对它不可见，`getppid()` 恒为 0，比对无从做起（照 `Some`
/// 那样比会把 guest 当场杀掉——实测 rc=137，正是这么踩出来的）。此时只设
/// PDEATHSIG，接受一个"fork 完到 prctl 之间父进程恰好死掉"的极小窗口：
/// 那几条指令之间中间进程刚 fork 完就死的概率可以忽略，而要彻底关掉它得
/// 额外引入一根 pipe 做存活检测，代价大于收益。
///
/// # Safety
/// 仅在 fork 后的子进程中调用；只做裸 syscall。
unsafe fn die_with_parent(expect_ppid: Option<libc::pid_t>) -> libc::c_int {
    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
        return -1;
    }
    if let Some(want) = expect_ppid {
        if libc::getppid() != want {
            // 父进程已经不在了：PDEATHSIG 不会来，自己退场，别当孤儿。
            libc::_exit(128 + libc::SIGKILL);
        }
    }
    0
}

/// pre_exec 闭包主体：建立命名空间并切根。返回 `Err` 会让 spawn 失败。
///
/// # Safety
/// 仅在 fork 后的子进程中调用；内部只做裸 syscall。
unsafe fn enter_namespaces(p: &NsPlan) -> std::io::Result<()> {
    let err = || std::io::Error::last_os_error();

    // 0. 生命周期绑定（L3）：把整棵进程树的存活挂在 wbox 身上，对齐 Windows
    //    侧 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 的语义承诺——wbox 退出或
    //    崩溃后不留孤儿。与限额不同，这条**没有开关**，两侧都是必开。
    if die_with_parent(Some(p.expect_ppid)) != 0 {
        return Err(err());
    }

    // 1. 限额必须在 unshare(CLONE_NEWUSER) **之前**落实：进了新 user
    //    namespace 后，宿主 cgroup 文件的写入会因 uid 映射被拒。
    apply_limits(p)?;

    // 1. 建 user + mount + pid (+ net) namespace。user namespace 让非 root 也能
    //    获得新 mount/net namespace 内的 CAP_SYS_ADMIN（rootless 的关键）。
    if libc::unshare(p.unshare_flags) != 0 {
        return Err(err());
    }
    // 2. 身份映射。顺序不能反：gid_map 之前必须 setgroups=deny。
    if write_proc_self(p.p_setgroups.as_ptr(), b"deny") != 0 {
        return Err(err());
    }
    if write_proc_self(p.p_uid_map.as_ptr(), &p.uid_map) != 0 {
        return Err(err());
    }
    if write_proc_self(p.p_gid_map.as_ptr(), &p.gid_map) != 0 {
        return Err(err());
    }

    // 3. PID namespace 只对**之后**创建的进程生效，故再 fork 一次：
    //    孙进程才是新 namespace 的 PID 1，中间进程只负责转发退出码。
    let pid = libc::fork();
    if pid < 0 {
        return Err(err());
    }
    if pid > 0 {
        // 中间进程：等孙进程结束，用同样的码退出。这里不能 return Ok——
        // 否则它会继续 exec，变成两份 guest 程序。
        let mut st: libc::c_int = 0;
        while libc::waitpid(pid, &mut st, 0) < 0 {
            if *libc::__errno_location() != libc::EINTR {
                libc::_exit(127);
            }
        }
        if libc::WIFEXITED(st) {
            libc::_exit(libc::WEXITSTATUS(st));
        }
        if libc::WIFSIGNALED(st) {
            libc::_exit(128 + libc::WTERMSIG(st));
        }
        libc::_exit(127);
    }

    // ---- 以下是孙进程（容器内 PID 1）----

    // PDEATHSIG 不会被 fork 继承，孙进程得自己再设一次，这次盯的是中间进程。
    // 链条因此是完整的：wbox 死 → 中间进程被 SIGKILL → 孙进程被 SIGKILL →
    // 孙进程是新 PID namespace 的 PID 1，它一死内核就清掉该 ns 内所有进程。
    // 少了这一环，杀 wbox 只能带走中间进程，guest 整棵树照样活着（实测如此）。
    //
    // 传 None：孙进程在新 PID ns 里，父进程不可见、getppid() 恒为 0，做不了
    // 那个比对（见 die_with_parent）。
    if die_with_parent(None) != 0 {
        return Err(err());
    }

    // 3b. 新 netns 里的 loopback 默认 DOWN，拉起来（见 bring_up_loopback）。
    if p.new_netns {
        bring_up_loopback();
    }

    // 4. 把根挂载点改成 private，否则后续挂载会传播回宿主。
    //    宿主程序模式下没有后续挂载，但仍然要做：mount namespace 只有在根被
    //    置为 private 后才真正与宿主解耦（否则 guest 自己的挂载会漏回宿主）。
    if libc::mount(
        p.c_empty.as_ptr(),
        p.c_root.as_ptr(),
        std::ptr::null(),
        libc::MS_REC | libc::MS_PRIVATE,
        std::ptr::null(),
    ) != 0
    {
        return Err(err());
    }

    // 宿主程序模式（LinuxMode::Host）：不换根，到此为止。
    let Some(r) = p.new_root.as_ref() else {
        return Ok(());
    };

    // 5. pivot_root 要求新根是一个挂载点，故把 rootfs bind 到自身。
    if libc::mount(
        r.rootfs.as_ptr(),
        r.rootfs.as_ptr(),
        std::ptr::null(),
        libc::MS_BIND | libc::MS_REC,
        std::ptr::null(),
    ) != 0
    {
        return Err(err());
    }
    // 5b. 最小设备集：逐个 bind 宿主设备节点到 rootfs/dev/*。
    //     必须在 pivot_root **之前**——切根后旧根 detach，宿主 /dev 就没了。
    //     单个设备失败不致命（例如 rootfs 只读时目标文件不存在），
    //     缺哪个由 guest 自己报错，比整个容器起不来好。
    for (src, dst) in &r.dev_binds {
        libc::mount(
            src.as_ptr(),
            dst.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        );
    }
    // 6. 切根。put_old 必须在新根之内。
    if libc::chdir(r.rootfs.as_ptr()) != 0 {
        return Err(err());
    }
    if libc::syscall(libc::SYS_pivot_root, r.rootfs.as_ptr(), r.put_old.as_ptr()) != 0 {
        return Err(err());
    }
    if libc::chdir(p.c_root.as_ptr()) != 0 {
        return Err(err());
    }
    // 7. 挂 /proc（新 PID namespace 的视图；ps/top 之类依赖它）。
    //    rootfs 里没有 /proc 目录时静默跳过——镜像不含它属正常情况，
    //    不该因此让整个容器起不来。
    libc::mount(
        r.c_proc_fstype.as_ptr(),
        r.c_proc.as_ptr(),
        r.c_proc_fstype.as_ptr(),
        0,
        std::ptr::null(),
    );
    // 8. 卸掉旧根：不 detach 的话宿主整个文件系统仍在容器内可见，隔离就是假的。
    if libc::umount2(r.c_put_old_in_new.as_ptr(), libc::MNT_DETACH) != 0 {
        return Err(err());
    }
    libc::rmdir(r.c_put_old_in_new.as_ptr());
    Ok(())
}

/// 把 CLI 的 `--memory`/`--cpu-pct`/`--max-procs` 落成本宿主可执行的限额方案。
///
/// 优先 cgroup v2；不可用时退化 setrlimit。**`--cpu-pct` 没有 rlimit 对应物**
/// （RLIMIT_CPU 限的是累计 CPU 秒数，不是占比），故此时明确报错而非静默忽略
/// ——PRD F5 一致性要求：宁可拒绝执行，也不能让同一条命令在不同宿主上
/// 隔离强度不同。
fn build_limit_plan(spec: &RunSpec) -> Result<LimitPlan> {
    build_limit_plan_with_cgroup(spec, cgroup2_self_dir())
}

/// `build_limit_plan` 的可注入版本：`cgroup_base` 显式传入而非现场探测，
/// 这样兜底逻辑能被**确定性**单测（传 `None` 即"本宿主没有 cgroup v2"），
/// 不受跑测机器的 cgroup 版本影响。真机行为另有一组不变量测试兜着。
fn build_limit_plan_with_cgroup(
    spec: &RunSpec,
    cgroup_base: Option<std::path::PathBuf>,
) -> Result<LimitPlan> {
    let l = &spec.limits;
    if l.memory_mb == 0 && l.cpu_pct == 0 && l.max_procs == 0 {
        return Ok(LimitPlan::None);
    }
    if let Some(p) = try_cgroup_plan(spec, cgroup_base)? {
        return Ok(p);
    }
    // ---- cgroup v2 不可用：rlimit 兜底 ----
    let nproc_wanted = l.max_procs > 0;
    if l.cpu_pct > 0 {
        return Err(WboxError::args(
            "本宿主无可用的 cgroup v2（或未开启委派），无法实施 --cpu-pct：\
             CPU 占比上限没有 setrlimit 对应物（RLIMIT_CPU 限的是累计 CPU 秒数，\
             语义不同）。请去掉 --cpu-pct，或在支持 cgroup v2 委派的宿主上运行。\
             wbox 不会静默忽略隔离参数",
        ));
    }
    // RLIMIT_NPROC **对特权进程不生效**（内核放行 root 绕过该上限）。实测：
    // 同一条 --max-procs 8 以 uid=1001 跑会 "can't fork"，以 root 跑则 40 个
    // 子进程全部起来。既然限不住，就不能静默接受这个参数——否则用户以为
    // 有进程数上限而实际没有，比直接报错危险得多（PRD F5）。
    if nproc_wanted && unsafe { libc::geteuid() } == 0 {
        return Err(WboxError::args(
            "以 root 运行且本宿主无可用 cgroup v2：无法实施 --max-procs——\
             RLIMIT_NPROC 对特权进程不生效（root 会绕过该上限），实测限不住。\
             请改以非 root 用户运行（rootless 正是 wbox 的推荐用法），\
             或在支持 cgroup v2 委派的宿主上运行。wbox 不会静默忽略隔离参数",
        ));
    }
    if spec.verbose {
        verbose_kv(
            "限额（rlimit 兜底）",
            "无 cgroup v2：--memory→RLIMIT_AS（限虚拟地址空间，偏严）、\
             --max-procs→RLIMIT_NPROC（按 uid 计）",
        );
    }
    Ok(LimitPlan::Rlimit {
        as_bytes: (l.memory_mb > 0).then(|| l.memory_mb * 1024 * 1024),
        nproc: (l.max_procs > 0).then(|| u64::from(l.max_procs)),
    })
}

/// 尝试落成 cgroup v2 方案；本宿主用不了就返回 `Ok(None)` 交给 rlimit 兜底。
///
/// **任何一步不可用都是 None 而不是 Err**，这一点是刻意的：
/// - 目录建不了 → 委派没开（GitHub ubuntu runner 就是这样）；
/// - `*.max` 写不进去 → 该控制器没在父级 `cgroup.subtree_control` 里启用。
///
/// 这两种都属于"本宿主没有可用的 cgroup v2"，而不是"用户请求有问题"。在这里
/// 硬报错会让 `--memory 16` 在那类机器上直接不能用，而 `RLIMIT_AS` 明明能兜住。
/// 真正该拒绝的情形（`--cpu-pct` 无对应物、root 下 `RLIMIT_NPROC` 限不住）由
/// 调用方在兜底分支判定——那里才知道兜底能不能满足请求。
///
/// 部分写成功后又失败时会把刚建的 cgroup 目录删掉：此时还没有任何进程加入，
/// 留着只是垃圾，且会让下次同 pid 的探测误判。
fn try_cgroup_plan(
    spec: &RunSpec,
    base: Option<std::path::PathBuf>,
) -> Result<Option<LimitPlan>> {
    let l = &spec.limits;
    let Some(base) = base else {
        return Ok(None);
    };
    let dir = base.join(format!("wbox-{}", std::process::id()));
    if std::fs::create_dir_all(&dir).is_err() {
        return Ok(None); // 委派未开
    }
    let abandon = || {
        let _ = std::fs::remove_dir(&dir);
        Ok(None)
    };

    let mut wrote = Vec::new();
    if l.memory_mb > 0 {
        let v = (l.memory_mb * 1024 * 1024).to_string();
        if std::fs::write(dir.join("memory.max"), &v).is_err() {
            return abandon();
        }
        wrote.push(format!("memory.max={}", v));
    }
    if l.max_procs > 0 {
        if std::fs::write(dir.join("pids.max"), l.max_procs.to_string()).is_err() {
            return abandon();
        }
        wrote.push(format!("pids.max={}", l.max_procs));
    }
    if l.cpu_pct > 0 {
        // cpu.max 格式："<quota_us> <period_us>"；period 取默认 100ms
        let period = 100_000u64;
        let quota = period * u64::from(l.cpu_pct) / 100;
        if std::fs::write(dir.join("cpu.max"), format!("{} {}", quota, period)).is_err() {
            return abandon();
        }
        wrote.push(format!("cpu.max={}/{}", quota, period));
    }
    if spec.verbose {
        verbose_kv("限额（cgroup v2）", wrote.join(" "));
        verbose_kv("cgroup", dir.display());
    }
    Ok(Some(LimitPlan::Cgroup {
        procs: cstr(&dir.join("cgroup.procs").to_string_lossy())?,
    }))
}

/// 把无符号整数写进栈上缓冲区，返回有效切片。pre_exec 内不能分配内存，
/// 故不能用 `format!`/`to_string`。
fn itoa(mut v: u64, buf: &mut [u8; 24]) -> usize {
    if v == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 24];
    let mut n = 0;
    while v > 0 {
        tmp[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
    }
    for i in 0..n {
        buf[i] = tmp[n - 1 - i];
    }
    n
}

/// 在 pre_exec 内落实限额。**必须在 unshare(CLONE_NEWUSER) 之前调用**：
/// 进了新 user namespace 后，宿主 cgroup 文件的写入会因 uid 映射而被拒。
///
/// # Safety
/// 仅在 fork 后的子进程中调用；只做裸 syscall。
unsafe fn apply_limits(p: &NsPlan) -> std::io::Result<()> {
    match &p.limits {
        LimitPlan::None => Ok(()),
        LimitPlan::Cgroup { procs } => {
            // 把自己加入 cgroup。此刻 guest 还没 exec，故不存在"先跑一会儿
            // 再被限制"的窗口（与 Windows 侧 CREATE_SUSPENDED→入 Job→Resume
            // 消除逃逸窗口是同一思路）。
            let mut buf = [0u8; 24];
            let n = itoa(libc::getpid() as u64, &mut buf);
            if write_proc_self(procs.as_ptr(), &buf[..n]) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
        LimitPlan::Rlimit { as_bytes, nproc } => {
            if let Some(v) = as_bytes {
                let rl = libc::rlimit { rlim_cur: *v, rlim_max: *v };
                if libc::setrlimit(libc::RLIMIT_AS, &rl) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            if let Some(v) = nproc {
                let rl = libc::rlimit { rlim_cur: *v, rlim_max: *v };
                if libc::setrlimit(libc::RLIMIT_NPROC, &rl) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Limits;

    fn spec_with(limits: Limits) -> RunSpec {
        RunSpec {
            name: "t".into(),
            limits,
            allow_network: false,
            keep_profile: false,
            workdir: std::env::temp_dir(),
            cmd: vec!["/bin/true".into()],
            env: vec![],
            verbose: false,
            env_pass_all: false,
        }
    }

    #[test]
    fn itoa_matches_to_string() {
        for v in [0u64, 1, 9, 10, 99, 100, 4095, 1_000_000, u64::MAX] {
            let mut b = [0u8; 24];
            let n = itoa(v, &mut b);
            assert_eq!(std::str::from_utf8(&b[..n]).unwrap(), v.to_string());
        }
    }

    #[test]
    fn no_limits_means_no_plan() {
        let plan = build_limit_plan(&spec_with(Limits::default())).unwrap();
        assert!(matches!(plan, LimitPlan::None));
    }

    // ---- 限额方案：两组测试各管一件事 ----
    // A) 注入 `None` 的确定性测试：钉死"没有 cgroup v2 时"的兜底逻辑，
    //    结果与跑测机器的 cgroup 版本无关。
    // B) 真机不变量测试：跑当前宿主的真实路径，只断言"结局必须落在可接受
    //    集合里"。缺了 B，像"这台机器上 --memory 会硬报错"这种缺陷没人发现；
    //    缺了 A，兜底数值就没有确定性覆盖。两组都要。

    /// A) `--cpu-pct` 在无可用 cgroup 时必须明确报错（PRD F5：
    /// 不静默忽略隔离参数）。
    #[test]
    fn cpu_pct_without_cgroup_errors() {
        let s = spec_with(Limits { memory_mb: 0, cpu_pct: 50, max_procs: 0 });
        let e = match build_limit_plan_with_cgroup(&s, None) {
            Err(e) => e,
            Ok(_) => panic!("无 cgroup 时 --cpu-pct 必须报错"),
        };
        let m = format!("{}", e);
        assert!(m.contains("cpu-pct"), "错误应点明是 --cpu-pct：{}", m);
    }

    /// A) 内存限额在无可用 cgroup 时应退化为 RLIMIT_AS。
    #[test]
    fn memory_falls_back_to_rlimit() {
        let s = spec_with(Limits { memory_mb: 16, cpu_pct: 0, max_procs: 0 });
        match build_limit_plan_with_cgroup(&s, None).unwrap() {
            LimitPlan::Rlimit { as_bytes, nproc } => {
                assert_eq!(as_bytes, Some(16 * 1024 * 1024));
                assert_eq!(nproc, None);
            }
            _ => panic!("无 cgroup 时内存限额应落成 Rlimit"),
        }
    }

    /// A) RLIMIT_NPROC 对 root 无效，因此无 cgroup 时 root 必须报错，非 root
    /// 才能退化为 RLIMIT_NPROC。
    #[test]
    fn max_procs_fallback_matches_privilege() {
        let s = spec_with(Limits { memory_mb: 0, cpu_pct: 0, max_procs: 8 });
        match build_limit_plan_with_cgroup(&s, None) {
            Err(e) if unsafe { libc::geteuid() } == 0 => {
                let m = format!("{}", e);
                assert!(m.contains("max-procs"), "错误应点明是 --max-procs：{}", m);
            }
            Ok(LimitPlan::Rlimit { as_bytes, nproc }) => {
                assert_eq!(as_bytes, None);
                assert_eq!(nproc, Some(8));
            }
            Err(e) => panic!("非 root 的进程限额应可退化为 rlimit：{}", e),
            Ok(_) => panic!("无 cgroup 时进程限额应落成 Rlimit"),
        }
    }

    /// B) `--cpu-pct` 在**当前宿主**上只有两种可接受结局：落成 cgroup v2
    /// 方案，或明确报错。绝不能是 rlimit/None —— `RLIMIT_CPU` 限的是累计
    /// CPU 秒数，不是占比，冒充等价就是骗人。
    ///
    /// 不按宿主特征分支：早先这里按 `cgroup2_self_dir()` 是否为 Some 来断言，
    /// 结果在 GitHub runner 上炸了 —— 那台机器有 cgroup v2、控制器齐全，但
    /// 委派没开，实际走的是兜底路径。"探测说有"与"实际能用"是两件事。
    #[test]
    fn cpu_pct_on_this_host_is_cgroup_or_error() {
        let s = spec_with(Limits { memory_mb: 0, cpu_pct: 50, max_procs: 0 });
        match build_limit_plan(&s) {
            Ok(LimitPlan::Cgroup { .. }) => {}
            Err(e) => {
                let m = format!("{}", e);
                assert!(m.contains("cpu-pct"), "错误应点明是 --cpu-pct：{}", m);
            }
            Ok(LimitPlan::Rlimit { .. }) => {
                panic!("--cpu-pct 不得退化成 rlimit：RLIMIT_CPU 限累计秒数，语义不同")
            }
            Ok(LimitPlan::None) => panic!("--cpu-pct 被静默忽略了，这违反 PRD F5"),
        }
    }

    /// B) `--memory` + `--max-procs` 在**当前宿主**上的三种可接受结局。
    /// 唯一与宿主有关的判据是 euid —— RLIMIT_NPROC 对特权进程不生效是
    /// **内核行为**，不是环境探测。
    #[test]
    fn memory_and_procs_on_this_host_never_silently_ignored() {
        let s = spec_with(Limits { memory_mb: 16, cpu_pct: 0, max_procs: 8 });
        let is_root = unsafe { libc::geteuid() } == 0;
        match build_limit_plan(&s) {
            Ok(LimitPlan::Cgroup { .. }) => {}
            Ok(LimitPlan::Rlimit { as_bytes, nproc }) => {
                assert!(!is_root, "以 root 退化到 RLIMIT_NPROC 等于限不住，必须报错");
                assert_eq!(as_bytes, Some(16 * 1024 * 1024));
                assert_eq!(nproc, Some(8));
            }
            Ok(LimitPlan::None) => panic!("--memory/--max-procs 被静默忽略了，这违反 PRD F5"),
            Err(e) => {
                let m = format!("{}", e);
                assert!(is_root, "非 root 下有 rlimit 兜底，不该报错：{}", m);
                assert!(m.contains("max-procs"), "错误应点明是 --max-procs：{}", m);
            }
        }
    }

    /// `cgroup2_self_dir()` 说有 v2，就必须真的有 `cgroup.controllers`；
    /// 但它**不保证可写**（委派可能没开），故不能拿它当"cgroup 可用"的判据
    /// —— 见 `try_cgroup_plan` 的注释。
    #[test]
    fn cgroup2_probe_reports_presence_not_usability() {
        if let Some(dir) = cgroup2_self_dir() {
            assert!(
                dir.join("cgroup.controllers").is_file(),
                "探测返回了 {} 但那里没有 cgroup.controllers",
                dir.display()
            );
        }
    }

    /// 网络默认必须**断开**（与 Windows 侧不授 INTERNET_CLIENT 一致），
    /// `--allow-network` 才共享宿主网络栈。这条是 PRD F5 一致性要求：
    /// 同一条命令在两个宿主上默认隔离强度必须相同。
    #[test]
    fn netns_is_created_unless_allow_network() {
        let deny = unshare_flags(false);
        let allow = unshare_flags(true);
        assert_ne!(
            deny & libc::CLONE_NEWNET,
            0,
            "默认必须新建 network namespace（Windows 侧默认无 INTERNET_CLIENT）"
        );
        assert_eq!(
            allow & libc::CLONE_NEWNET,
            0,
            "--allow-network 时不应再建 netns（否则连不上外网）"
        );
        // 其余三种 namespace 与网络选项无关，两种情况都必须有
        for f in [deny, allow] {
            for (bit, n) in [
                (libc::CLONE_NEWUSER, "user"),
                (libc::CLONE_NEWNS, "mount"),
                (libc::CLONE_NEWPID, "pid"),
            ] {
                assert_ne!(f & bit, 0, "{} namespace 不该受 --allow-network 影响", n);
            }
        }
    }

    /// 三种封禁 userns 的方式都要能认出来，并且各自给出**能照做**的解法。
    /// 起因：Ubuntu 24.04（含 GitHub ubuntu runner）默认由 AppArmor 关掉
    /// unprivileged userns，而 wbox 那时只打印 "Operation not permitted"，
    /// 用户既看不出原因也不知道怎么办。
    #[test]
    fn userns_hint_recognizes_each_restriction() {
        let apparmor = userns_hint_from(Some("1\n"), None, None).expect("应认出 AppArmor 限制");
        assert!(apparmor.contains("apparmor_restrict_unprivileged_userns"));
        assert!(apparmor.contains("sysctl"), "提示必须给出可照做的命令");

        let clone = userns_hint_from(None, Some("0\n"), None).expect("应认出 unprivileged_userns_clone=0");
        assert!(clone.contains("unprivileged_userns_clone"));

        let maxns = userns_hint_from(None, None, Some("0\n")).expect("应认出配额为 0");
        assert!(maxns.contains("max_user_namespaces"));

        // 优先级：AppArmor 最常见，同时命中时先报它
        let both = userns_hint_from(Some("1"), Some("0"), Some("0")).unwrap();
        assert!(both.contains("AppArmor"), "同时命中时应优先报 AppArmor：{}", both);
    }

    /// 宿主没有任何限制时不能瞎提示——否则会把"命令不存在"这类无关错误
    /// 引到错误的方向上去。
    #[test]
    fn userns_hint_silent_when_unrestricted() {
        assert!(userns_hint_from(Some("0"), Some("1"), Some("31231")).is_none());
        assert!(userns_hint_from(None, None, None).is_none(), "文件都不存在时不该提示");
    }

    /// 只有权限类错误才去查 sysctl；其它错误原样返回，不添乱。
    #[test]
    fn spawn_hint_only_for_permission_errors() {
        use std::io::{Error, ErrorKind};
        assert_eq!(
            spawn_failure_hint(&Error::new(ErrorKind::NotFound, "no such file")),
            "",
            "命令不存在与 userns 无关，不该扯上 sysctl"
        );
    }

    /// `IfReqFlags` 必须与内核 `struct ifreq` 同大小，否则 SIOCSIFFLAGS
    /// 会读到越界数据（ioctl 按固定布局解析，不看我们声明了多少字段）。
    #[test]
    fn ifreq_layout_matches_kernel() {
        assert_eq!(
            std::mem::size_of::<IfReqFlags>(),
            std::mem::size_of::<libc::ifreq>(),
            "IfReqFlags 与 libc::ifreq 大小不一致，SIOCSIFFLAGS 布局会错"
        );
    }
}
