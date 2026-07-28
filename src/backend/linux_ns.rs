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
use super::limits::{apply_limits, build_limit_plan, LimitPlan};
use super::LinuxMode;
use crate::error::{Result, WboxError};
use std::ffi::CString;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// 把路径转成 fork 前就备好的 `CString`（闭包内不再分配）。
pub(super) fn cstr(s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| WboxError::spawn(format!("路径含 NUL 字节：{:?}", s)))
}

/// 写 `/proc/self/<name>`，只用裸 syscall（async-signal-safe）。
/// 返回 0 成功、-1 失败（errno 已由 libc 设置）。
///
/// # Safety
/// `path`/`data` 必须是有效的 NUL 结尾指针，且在调用期间保持有效。
pub(super) unsafe fn write_proc_self(path: *const libc::c_char, data: &[u8]) -> libc::c_int {
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
    /// 卷挂载：(宿主源, 容器内目标的**宿主可见路径**, 只读)。
    /// 目标在 fork 前就解析成宿主视角的绝对路径——镜像模式是 `rootfs/<guest>`，
    /// 宿主模式就是 `<guest>`；闭包内不再拼字符串（不能分配）。
    vol_binds: Vec<(CString, CString, bool)>,
    /// `/proc/self/setgroups` / `uid_map` / `gid_map`
    p_setgroups: CString,
    p_uid_map: CString,
    p_gid_map: CString,
    uid_map: Vec<u8>,
    gid_map: Vec<u8>,
    c_root: CString,
    /// 换根后要 `chdir` 进去的容器内目录（`-w` 或镜像的 `WorkingDir`）。
    /// `None` = 落在 `/`（换根后的默认）。
    c_guest_workdir: Option<CString>,
    /// 上面那个目录的**逐级前缀**，用于换根后按需 `mkdir`（docker 在工作目录
    /// 不存在时会建出来，这里对齐）。
    ///
    /// **必须换根之后再建**：换根前 `/` 还是宿主视角，往 `rootfs/<w>` 里建会写进
    /// **共享的镜像缓存**——那正是 F9.12 花力气避免的污染。换根后 `/` 已是 overlay
    /// 合并视图，写入自然落在本容器的 upper 里。
    ///
    /// 前缀在 fork 前算好：子进程里不能分配内存。
    c_guest_workdir_parents: Vec<CString>,
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
    /// capability 裁剪计划（PRD F9.8），fork 前算好
    caps: CapPlan,
    /// 容器内主机名（UTS namespace 已隔离，见 unshare_flags）。
    /// 空 = 不设（加入了 peer 的 UTS 时不该改它的主机名）。
    hostname: Vec<u8>,
    /// seccomp 过滤器程序（PRD F9.9），fork 前构造好。空 = 不装。
    /// 子进程里不能分配，BPF 指令序列必须在这之前就备齐。
    seccomp: Vec<libc::sock_filter>,
    /// `--network container:<NAME>`（PRD F9.11）：加入既有容器的 namespace，
    /// 而不是自建。fd 在 fork 前打开——子进程里不能做路径解析。
    ///
    /// **user 与 net 必须一起加入**。netns 的 setns 要求在其**属主 userns**
    /// 里有 CAP_SYS_ADMIN；自建 userns 后与目标的 userns 互为兄弟，拿不到
    /// 那个权限。先 setns 进目标 userns（同宿主 uid 即有权限），才进得了它的
    /// netns——与 `wbox exec` 是同一条已验证的路径。
    join_ns: Option<JoinNs>,
}

/// `--network container:` 模式要加入的目标 namespace 文件句柄。
struct JoinNs {
    /// 目标容器的 user namespace。**永远第一个进**：其余 namespace 的 setns
    /// 都要求在其属主 userns 内有 CAP_SYS_ADMIN。
    user: std::fs::File,
    /// 要加入的其余 namespace：(fd, CLONE_* 标志)。哪些进来由调用方决定，
    /// 这里不写死——`--network`/`--ipc`/`--uts container:` 共用同一套机制，
    /// 各写一份迟早会在"先进 userns"这条关键顺序上漂移。
    others: Vec<(std::fs::File, libc::c_int)>,
}

/// capability 裁剪的**预算好**形式。fork 后的子进程不能分配、不能读文件，
/// 所以 `/proc/sys/kernel/cap_last_cap` 与要丢弃的位号清单都在这里备齐。
struct CapPlan {
    /// 用户没动过 capability 时为 false，落地层整段跳过——不为"什么都不改"
    /// 付出任何行为变化的风险。
    active: bool,
    /// 要从 **bounding set** 移除的位号。移除 bounding set 才是持久保证：
    /// 它决定 execve 之后还能不能重新拿到这个 capability。
    bounding_drop: Vec<u32>,
    /// capset 的保留掩码（低 32 位 / 高 32 位）
    keep_low: u32,
    keep_high: u32,
}

/// `capset(2)` 的头部。libc crate 没有导出这两个结构，故按
/// `include/uapi/linux/capability.h` 就地声明。
#[repr(C)]
struct CapUserHeader {
    version: u32,
    pid: libc::c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CapUserData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

/// `_LINUX_CAPABILITY_VERSION_3`：64 位 capability 集合，两组 32 位。
const CAP_VERSION_3: u32 = 0x2008_0522;

/// 内核认识的最高 capability 位号。读不到就退回本地表的上界——宁可少丢，
/// 也不要凭猜测丢掉用户没要求丢的东西。
fn kernel_last_cap() -> u32 {
    std::fs::read_to_string("/proc/sys/kernel/cap_last_cap")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(crate::caps::CAP_NAMES.len() as u32 - 1)
        .min(63)
}

/// rootless overlay 在本机是否可用。供 build 决定能不能用硬链接铺基础层
/// （PRD L5b）——不能的话 RUN 会就地写坏共享 inode，只能退回整份复制。
pub(crate) fn rootless_overlay_available() -> bool {
    let scratch = std::env::temp_dir().join(format!("wbox-ovchk-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&scratch);
    let ok = probe_rootless_overlay(&scratch);
    let _ = std::fs::remove_dir_all(&scratch);
    ok
}

/// 由策略算出落地计划。
fn build_cap_plan(policy: &crate::caps::CapPolicy) -> CapPlan {
    if policy.is_default() {
        return CapPlan {
            active: false,
            bounding_drop: Vec::new(),
            keep_low: 0,
            keep_high: 0,
        };
    }
    let last = kernel_last_cap();
    let bounding_drop = (0..=last).filter(|i| !policy.keeps(*i)).collect();
    CapPlan {
        active: true,
        bounding_drop,
        keep_low: policy.mask_low(),
        keep_high: policy.mask_high(last),
    }
}

/// 丢弃 capability。**必须在所有特权操作之后**：前面的 mount / pivot_root
/// 都要 userns 内的 `CAP_SYS_ADMIN`，先丢就起不来了。
///
/// # Safety
/// 仅在 fork 后的子进程中调用；只做裸 syscall，不分配。
unsafe fn apply_caps(p: &CapPlan) -> std::io::Result<()> {
    if !p.active {
        return Ok(());
    }
    // 1. 先削 bounding set。这一步需要 effective 集里还有 CAP_SETPCAP，
    //    所以必须排在 capset 之前——顺序反了就只削掉一部分然后失败。
    //    单个位号失败不致命：内核不认识的位返回 EINVAL，跳过即可。
    for bit in &p.bounding_drop {
        libc::prctl(libc::PR_CAPBSET_DROP, *bit as libc::c_ulong, 0, 0, 0);
    }
    // 2. 清空 ambient 集，否则 execve 后它会把 capability 重新抬进 permitted。
    libc::prctl(
        libc::PR_CAP_AMBIENT,
        libc::PR_CAP_AMBIENT_CLEAR_ALL as libc::c_ulong,
        0,
        0,
        0,
    );
    // 3. 真正收窄当前进程的三个集合。只削 bounding set 是不够的——那只挡住
    //    "execve 之后重新获得"，当前进程手里的 effective 仍然在。
    let hdr = CapUserHeader {
        version: CAP_VERSION_3,
        pid: 0,
    };
    let data = [
        CapUserData {
            effective: p.keep_low,
            permitted: p.keep_low,
            inheritable: p.keep_low,
        },
        CapUserData {
            effective: p.keep_high,
            permitted: p.keep_high,
            inheritable: p.keep_high,
        },
    ];
    if libc::syscall(libc::SYS_capset, &hdr as *const _, data.as_ptr()) != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// overlay 可写层（PRD F9.12）：挂在 rootfs 路径**自身**之上。
///
/// 挂在 lowerdir 自己头上是 overlayfs 的既定用法（挂载时即持有底层 dentry
/// 引用），好处是**所有既有路径都不用改**——卷挂载目标、设备 bind、pivot_root
/// 全都照旧指向 rootfs 路径，挂上之后看到的自动是合并视图。
struct OverlayPlan {
    /// "overlay"（fstype 与 source 同一个串）
    c_overlay: CString,
    /// `lowerdir=...,upperdir=...,workdir=...`
    opts: CString,
}

/// 把 `/a/b/c` 拆成 `["/a", "/a/b", "/a/b/c"]`，供换根后逐级 `mkdir`。
///
/// 在 fork 前算好：子进程里不能分配内存（那是 `pre_exec` 的硬约束）。
fn guest_dir_prefixes(dir: &str) -> Result<Vec<CString>> {
    let mut out = Vec::new();
    let mut acc = String::new();
    for seg in dir.split('/').filter(|s| !s.is_empty() && *s != ".") {
        // `..` 不该出现在工作目录里；出现了多半是配置错误，直接拒绝而不是消解——
        // 消解出来的路径与用户写的不是同一个，出问题时对不上。
        if seg == ".." {
            return Err(crate::error::WboxError::args(format!(
                "工作目录 '{}' 含 '..'，请给出规范的绝对路径",
                dir
            )));
        }
        acc.push('/');
        acc.push_str(seg);
        out.push(cstr(&acc)?);
    }
    Ok(out)
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
    /// `Some` = 把容器写入引到 per-container 的 upper 层（镜像缓存保持只读）。
    /// `None` = 共享写入（内核不支持 rootless overlay 时的回退，parent 已告警）。
    overlay: Option<OverlayPlan>,
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
        LinuxMode::Image => {
            // overlay 可写层（F9.12）：把容器写入引到 per-container 的 upper，
            // 镜像缓存保持只读。upper/work 放在容器状态目录里，随 `rm` 一并
            // 清理；detached 容器退出后还能翻 upper 看它改了什么。
            // WBOX_NO_OVERLAY=1 是逃生口（例如想沿用旧的共享写入语义）。
            let overlay = if spec.direct_rootfs_writes {
                // build 的 RUN 步骤：写入就是产物，直落 rootfs（见 RunSpec 字段说明）。
                None
            } else if std::env::var_os("WBOX_NO_OVERLAY").is_some() {
                if spec.verbose {
                    super::super::verbose_kv("rootfs 写入层", "共享写入（WBOX_NO_OVERLAY=1）");
                }
                None
            } else {
                let layer_dir = match spec.overlay_layer_dir.as_ref() {
                    Some(d) => d.clone(),
                    None => crate::runstate::dir_for(&spec.name)?.join("layer"),
                };
                build_overlay(&prepared.workdir.to_string_lossy(), &layer_dir, spec.verbose)
            };
            Some(build_new_root(prepared, overlay)?)
        }
    };

    // 网络：默认**不给**（新建空 network namespace），与 Windows 侧不授
    // INTERNET_CLIENT 能力的默认一致。PRD F5 一致性要求同一条命令
    // 在两个宿主上默认隔离强度相同，故这里不能"Linux 上顺便继承宿主网络"。
    let joining = spec.network_container.is_some()
        || spec.ipc_container.is_some()
        || spec.uts_container.is_some();
    // 加入模式：网络来自 peer（不建也不共享宿主），loopback 由 peer 拉起过。
    let new_netns = !spec.allow_network && spec.network_container.is_none() && !joining;
    let unshare_flags = if joining {
        // 身份来自 peer，**不能**再 CLONE_NEWUSER：那会把自己关进一个与 peer
        // 的 namespace 无权限关系的兄弟 userns。自建的只有 mount+pid，
        // 外加那些**没有**从 peer 加入的 namespace——加入哪个就不自建哪个，
        // 否则 unshare 会立刻把刚 setns 进去的那个覆盖掉。
        let mut f = libc::CLONE_NEWNS | libc::CLONE_NEWPID;
        if spec.network_container.is_none() && !spec.allow_network {
            f |= libc::CLONE_NEWNET;
        }
        if spec.ipc_container.is_none() {
            f |= libc::CLONE_NEWIPC;
        }
        if spec.uts_container.is_none() {
            f |= libc::CLONE_NEWUTS;
        }
        f
    } else {
        unshare_flags(spec.allow_network)
    };

    // rootless 映射：把宿主当前 uid/gid 映射成容器内的 root。
    // 写 gid_map 之前必须先写 setgroups=deny，否则内核拒绝（CVE-2014-8989 之后的加固）。
    let limits = build_limit_plan(spec)?;
    // cgroup 目录要在 guest 退出后删掉。plan 稍后会被 pre_exec 闭包 move 走，
    // 故先把清单复制出来。cgroup v2 不会自动回收空目录，不删就是每跑一次漏一个。
    let cgroup_cleanup: Vec<std::path::PathBuf> = match &limits {
        LimitPlan::Cgroup { cleanup, .. } => cleanup.clone(),
        _ => Vec::new(),
    };
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    // `--user`：rootless 下没有 newuidmap/newgidmap 就**只能映射一个 id**，
    // 但那一个映射到容器内的哪个号是自由的。所以 `--user 1000` 的实现不是
    // "先当 root 再 setuid"（那需要额外映射目标 uid），而是直接把宿主这唯一
    // 的 uid 映射成 1000。副作用要说清：容器内除了它以外的所有 uid 都是
    // overflow（nobody），chown 到别的号会 EINVAL。
    let (target_uid, target_gid) = match spec.user {
        Some(u) => (u.uid, u.gid),
        None => (0, 0),
    };
    // 卷挂载目标解析成宿主视角路径：镜像模式挂进 rootfs 内，宿主模式直接用
    // guest 路径（宿主模式不换根，两者本就同一命名空间）。
    let mut vol_binds = Vec::new();
    for v in &spec.volumes {
        let target = match new_root.as_ref() {
            // guest 以 '/' 开头，join 会当成绝对路径覆盖掉 rootfs，故去掉前导 '/'
            Some(_) => prepared.workdir.join(v.guest.trim_start_matches('/')),
            None => std::path::PathBuf::from(&v.guest),
        };
        // 挂载点必须先存在。镜像里没有这个目录是常态（例如挂 /data），
        // 这里替用户建**容器内**的挂载点是安全的：它在 rootfs 里，不是宿主。
        let _ = std::fs::create_dir_all(&target);
        vol_binds.push((
            cstr(&v.host.to_string_lossy())?,
            cstr(&target.to_string_lossy())?,
            v.read_only,
        ));
    }

    // --network container:<peer>：fork 前解析目标并打开其 namespace fd。
    // 打开的 fd 本身会钉住 namespace——即使 peer 随后退出，加入方也不会踩空。
    // `--network`/`--ipc`/`--uts container:<NAME>`：加入既有容器的 namespace。
    // 三者共用同一条解析与 setns 路径（见 JoinNs 的说明）。
    let join_ns = resolve_join(spec)?;
    let plan = NsPlan {
        vol_binds,
        join_ns,
        p_setgroups: cstr("/proc/self/setgroups")?,
        p_uid_map: cstr("/proc/self/uid_map")?,
        p_gid_map: cstr("/proc/self/gid_map")?,
        uid_map: format!("{} {} 1\n", target_uid, uid).into_bytes(),
        gid_map: format!("{} {} 1\n", target_gid, gid).into_bytes(),
        c_root: cstr("/")?,
        // 只在换根的镜像模式下有意义；宿主程序模式用 Command::current_dir。
        c_guest_workdir: match (mode, spec.guest_workdir.as_deref()) {
            (LinuxMode::Image, Some(w)) if !w.is_empty() => Some(cstr(w)?),
            _ => None,
        },
        c_guest_workdir_parents: match (mode, spec.guest_workdir.as_deref()) {
            (LinuxMode::Image, Some(w)) if !w.is_empty() => guest_dir_prefixes(w)?,
            _ => Vec::new(),
        },
        c_empty: cstr("")?,
        unshare_flags,
        new_netns,
        expect_ppid: unsafe { libc::getpid() },
        new_root,
        limits,
        caps: build_cap_plan(&spec.caps),
        // 加入了 peer 的 UTS 时不设主机名——那是 peer 的，改它属于越界。
        hostname: if spec.uts_container.is_some() {
            Vec::new()
        } else {
            // 默认用容器名，与 docker 一致（它用容器 ID 的前 12 位）。
            // 主机名有长度上限，超了就截断而不是失败：一个太长的容器名不该
            // 让容器起不来。
            let h = spec.hostname.clone().unwrap_or_else(|| spec.name.clone());
            h.into_bytes().into_iter().take(64).collect()
        },
        seccomp: if spec.seccomp.is_default() {
            Vec::new()
        } else {
            crate::seccomp::build_filter(&spec.seccomp)
        },
    };

    if spec.verbose {
        let ns_list = if joining {
            "mount+pid namespace（user+net 来自 --network container: 的目标）"
        } else if new_netns {
            "user+mount+pid+net namespace（rootless）"
        } else {
            "user+mount+pid namespace（rootless，共享宿主网络）"
        };
        verbose_kv("隔离", ns_list);
        verbose_kv(
            "uid 映射",
            format!("容器 {} ← 宿主 {}（gid {} ← {}）", target_uid, uid, target_gid, gid),
        );
        verbose_kv("capability", spec.caps.summary());
        verbose_kv("seccomp", spec.seccomp.summary());
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
    // guest 已退出 → 它的 cgroup 现在是空的，可以删。顺序要紧：先删 guest
    // target，再删 wbox 自己的 supervisor（wbox 此刻还在 supervisor 里，
    // rmdir 一个非空 cgroup 会失败，这属正常，忽略即可——进程退出后由宿主
    // 或下一次运行回收）。
    for d in &cgroup_cleanup {
        let _ = std::fs::remove_dir(d);
    }
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
/// 解析三个 `container:<NAME>` 选项，打开要加入的 namespace fd。
///
/// **所有目标必须是同一个容器**：加入模式先 `setns` 进目标的 user namespace，
/// 而一个进程只能在一个 userns 里。指向不同容器时明确报错，而不是挑一个赢家
/// ——挑赢家等于悄悄忽略另一个，用户会以为两个都生效了。
fn resolve_join(spec: &RunSpec) -> Result<Option<JoinNs>> {
    let wanted: [(Option<&str>, &str, libc::c_int); 3] = [
        (spec.network_container.as_deref(), "net", libc::CLONE_NEWNET),
        (spec.ipc_container.as_deref(), "ipc", libc::CLONE_NEWIPC),
        (spec.uts_container.as_deref(), "uts", libc::CLONE_NEWUTS),
    ];
    let peers: Vec<&str> = wanted.iter().filter_map(|(p, _, _)| *p).collect();
    let Some(&peer) = peers.first() else {
        return Ok(None);
    };
    if peers.iter().any(|p| *p != peer) {
        return Err(crate::error::WboxError::args(
            "--network/--ipc/--uts container: 指向了不同的容器：加入模式要先进入目标的 \
             user namespace，而一个进程只能在一个 user namespace 里，故三者必须指向同一个容器",
        ));
    }
    let peer_dir = crate::runstate::resolve_existing(peer)?;
    if crate::runstate::liveness(&peer_dir) == crate::runstate::Liveness::Exited {
        return Err(crate::error::WboxError::args(format!(
            "container:{}：目标容器已退出，没有可加入的 namespace",
            peer
        )));
    }
    // 有界等待 peer 的 container.pid 落盘。**不能一看没有就报错**：
    // `run -d` 在 supervisor 起来那一刻就返回，而 container.pid 由后台轮询
    // /proc 的记录器稍后写入——中间有个真实的竞态窗口，compose 顺序启动服务时
    // 必然撞上（实测第二个服务直接退出）。让调用方"稍后重试"是把我们的竞态
    // 转嫁给用户。
    let pid = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(p) = crate::runstate::container_pid(&peer_dir) {
                break p;
            }
            if crate::runstate::liveness(&peer_dir) == crate::runstate::Liveness::Exited {
                return Err(crate::error::WboxError::args(format!(
                    "container:{}：目标容器在等待其 namespace 就绪期间退出了",
                    peer
                )));
            }
            if std::time::Instant::now() >= deadline {
                return Err(crate::error::WboxError::args(format!(
                    "container:{}：等待 5 秒后目标仍未记录容器 pid",
                    peer
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    let open = |ns: &str| {
        std::fs::File::open(format!("/proc/{}/ns/{}", pid, ns)).map_err(|e| {
            crate::error::WboxError::spawn(format!(
                "container:{}：打开目标 namespace '{}' 失败：{}（容器可能刚退出）",
                peer, ns, e
            ))
        })
    };
    let mut others = Vec::new();
    for (want, ns, flag) in wanted {
        if want.is_some() {
            others.push((open(ns)?, flag));
        }
    }
    Ok(Some(JoinNs {
        user: open("user")?,
        others,
    }))
}

fn unshare_flags(allow_network: bool) -> libc::c_int {
    // IPC 与 UTS 也要隔离：docker/podman 默认给每个容器独立的这两种 namespace，
    // 而此前 wbox 让容器直接**共享宿主的**——guest 能看见并操作宿主的 System V
    // 信号量/共享内存段，也能 sethostname 改掉宿主主机名。这是一个真实的隔离
    // 缺口，不是"少一个特性"。rootless 下拿得到：新 userns 里有 CAP_SYS_ADMIN。
    let mut f = libc::CLONE_NEWUSER
        | libc::CLONE_NEWNS
        | libc::CLONE_NEWPID
        | libc::CLONE_NEWIPC
        | libc::CLONE_NEWUTS;
    if !allow_network {
        f |= libc::CLONE_NEWNET;
    }
    f
}

/// 备好镜像模式的换根参数（在 fork **之前**做完所有分配与目录准备）。
/// 探测本内核是否允许 userns 内挂 overlayfs（5.11 起）。
///
/// 在**父进程侧**探明，而不是让容器启动失败后靠 errno 猜：mount 的 EINVAL
/// 什么都说明不了。探测就是真做一次最小 overlay 挂载（独立 userns+mountns，
/// 对外零副作用），结果按实际发生的事判定——这类"探测说有、实际不能用"的坑
/// 这个项目已经踩过（cgroup v2 那次）。
fn probe_rootless_overlay(scratch: &std::path::Path) -> bool {
    let base = scratch.join(".ovprobe");
    for d in ["lower", "upper", "work", "merged"] {
        if std::fs::create_dir_all(base.join(d)).is_err() {
            return false;
        }
    }
    let (Ok(opts), Ok(merged), Ok(ov), Ok(root), Ok(empty), Ok(p_sg), Ok(p_uid), Ok(p_gid)) = (
        cstr(&format!(
            "userxattr,lowerdir={0}/lower,upperdir={0}/upper,workdir={0}/work",
            base.to_string_lossy()
        )),
        cstr(&base.join("merged").to_string_lossy()),
        cstr("overlay"),
        cstr("/"),
        cstr(""),
        cstr("/proc/self/setgroups"),
        cstr("/proc/self/uid_map"),
        cstr("/proc/self/gid_map"),
    ) else {
        return false;
    };
    // 必须写 uid/gid 映射：光有 capability 不够——upper/work 目录的属主在
    // 未映射的 userns 里是 overflow uid，过不了 overlayfs 的属主校验。
    // 首次实测正是这么失败的（手工 `unshare -Umr` 探测通过、进程内探测不过，
    // 差的就是 -r 那份映射）。
    let uid_map = format!("0 {} 1\n", unsafe { libc::getuid() }).into_bytes();
    let gid_map = format!("0 {} 1\n", unsafe { libc::getgid() }).into_bytes();
    let ok = unsafe {
        let pid = libc::fork();
        if pid < 0 {
            return false;
        }
        if pid == 0 {
            // 子进程：进独立 userns+mountns 试挂一次。不写 uid_map——
            // unshare(NEWUSER) 后立即就有本 ns 的全部 capability，
            // 而 overlay 挂载只看 capable(CAP_SYS_ADMIN)。
            if libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) != 0 {
                libc::_exit(1);
            }
            if write_proc_self(p_sg.as_ptr(), b"deny") != 0
                || write_proc_self(p_uid.as_ptr(), &uid_map) != 0
                || write_proc_self(p_gid.as_ptr(), &gid_map) != 0
            {
                libc::_exit(1);
            }
            // 根置 private，防止试挂传播回宿主
            libc::mount(
                empty.as_ptr(),
                root.as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            );
            let rc = libc::mount(
                ov.as_ptr(),
                merged.as_ptr(),
                ov.as_ptr(),
                0,
                opts.as_ptr() as *const libc::c_void,
            );
            libc::_exit(if rc == 0 { 0 } else { 1 });
        }
        let mut st: libc::c_int = 0;
        while libc::waitpid(pid, &mut st, 0) < 0 {
            if *libc::__errno_location() != libc::EINTR {
                return false;
            }
        }
        libc::WIFEXITED(st) && libc::WEXITSTATUS(st) == 0
    };
    let _ = std::fs::remove_dir_all(&base);
    ok
}

/// 备好 overlay 可写层计划。返回 `None` 表示走共享写入回退（原因已打印）。
fn build_overlay(rootfs: &str, layer_dir: &std::path::Path, verbose: bool) -> Option<OverlayPlan> {
    // overlayfs 的选项串用 ',' 分隔、':' 分隔多个 lower，路径里含这两个字符
    // 就没法表达。宁可回退也不要错挂——挂错 lower 的表现是"容器看到错的根"。
    let layer = layer_dir.to_string_lossy().into_owned();
    if rootfs.contains([',', ':']) || layer.contains([',', ':']) {
        eprintln!(
            "wbox: 路径含 ',' 或 ':'，overlayfs 选项无法表达，回退共享写入（容器写入会落在镜像缓存里）"
        );
        return None;
    }
    for d in ["upper", "work"] {
        if std::fs::create_dir_all(layer_dir.join(d)).is_err() {
            eprintln!("wbox: 无法创建 overlay 层目录，回退共享写入");
            return None;
        }
    }
    if !probe_rootless_overlay(layer_dir) {
        // 回退必须**出声**：静默降级会让用户以为镜像缓存是只读的。
        eprintln!(
            "wbox: 本内核不支持 rootless overlay（需 5.11+），回退共享写入：容器对 / 的写入会留在镜像缓存中"
        );
        return None;
    }
    // `userxattr` 是 rootless overlay 的必需项（内核 5.11+）：不加的话
    // overlay 要写 `trusted.overlay.*` xattr 来标记 opaque 目录，而那需要
    // **初始 userns** 里的 CAP_SYS_ADMIN——rootless 拿不到。后果很具体：
    // 容器里 `rm -rf` 一个镜像自带的目录会直接 EIO 失败（实测），
    // 而删文件却是好的，所以不测目录就发现不了。
    let opts = cstr(&format!(
        "userxattr,lowerdir={},upperdir={}/upper,workdir={}/work",
        rootfs, layer, layer
    ))
    .ok()?;
    if verbose {
        super::super::verbose_kv("rootfs 写入层", format!("overlay（upper 在 {}/upper）", layer));
    }
    Some(OverlayPlan {
        c_overlay: cstr("overlay").ok()?,
        opts,
    })
}

fn build_new_root(prepared: &Prepared, overlay: Option<OverlayPlan>) -> Result<NewRoot> {
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
        overlay,
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
    setup_namespaces(p)?;
    // capability 放在**最后**丢：上面每一步 mount/pivot_root 都要 CAP_SYS_ADMIN。
    // 收在这个包装里而不是散在两个 return 分支上——宿主模式与镜像模式各有一个
    // 出口，逐个补调用迟早会漏掉一个，而漏掉的那次是"以为收紧了其实没有"。
    apply_caps(&p.caps)?;
    // seccomp 排在 capability 之后：过滤器可能把 capset 本身也拦了，
    // 顺序反过来就会让 --cap-drop 静默失效。
    crate::seccomp::apply(&p.seccomp)
}

/// [`enter_namespaces`] 的主体：建立命名空间并切根（不含 capability 裁剪）。
///
/// # Safety
/// 同 [`enter_namespaces`]。
unsafe fn setup_namespaces(p: &NsPlan) -> std::io::Result<()> {
    let err = || std::io::Error::last_os_error();

    // 0. 生命周期绑定（L3）：把整棵进程树的存活挂在 wbox 身上，对齐 Windows
    //    侧 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 的语义承诺——wbox 退出或
    //    崩溃后不留孤儿。与限额不同，这条**没有开关**，两侧都是必开。
    if die_with_parent(Some(p.expect_ppid)) != 0 {
        return Err(err());
    }

    // 1. 限额必须在 unshare(CLONE_NEWUSER) **之前**落实：进了新 user
    //    namespace 后，宿主 cgroup 文件的写入会因 uid 映射被拒。
    apply_limits(&p.limits)?;

    // 1b. `--network container:` 加入模式：先进目标的 user+net namespace，
    //     **必须在 unshare 之前**。顺序的道理见 JoinNs 的说明——netns 的
    //     setns 要求在其属主 userns 内有 CAP_SYS_ADMIN。
    if let Some(j) = p.join_ns.as_ref() {
        use std::os::unix::io::AsRawFd;
        if libc::setns(j.user.as_raw_fd(), libc::CLONE_NEWUSER) != 0 {
            return Err(err());
        }
        for (f, flag) in &j.others {
            if libc::setns(f.as_raw_fd(), *flag) != 0 {
                return Err(err());
            }
        }
    }

    // 2. 建 user + mount + pid (+ net) namespace。user namespace 让非 root 也能
    //    获得新 mount/net namespace 内的 CAP_SYS_ADMIN（rootless 的关键）。
    //    加入模式下 flags 里没有 CLONE_NEWUSER/NEWNET——身份与网络都来自 peer。
    if libc::unshare(p.unshare_flags) != 0 {
        return Err(err());
    }
    // 3. 身份映射。顺序不能反：gid_map 之前必须 setgroups=deny。
    //    加入模式跳过：peer 的 userns 已写过映射，重写会 EPERM。
    if p.join_ns.is_none() {
        if write_proc_self(p.p_setgroups.as_ptr(), b"deny") != 0 {
            return Err(err());
        }
        if write_proc_self(p.p_uid_map.as_ptr(), &p.uid_map) != 0 {
            return Err(err());
        }
        if write_proc_self(p.p_gid_map.as_ptr(), &p.gid_map) != 0 {
            return Err(err());
        }
    }

    // 4. PID namespace 只对**之后**创建的进程生效，故再 fork 一次：
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

    // 4a. 设置容器内主机名。UTS namespace 已在 unshare 时隔离，故这一步
    //     **不会**碰到宿主的主机名。放在双 fork 之后：孙进程才是最终的 guest
    //     宿主（中间进程只转发退出码），但 UTS 是从中间进程继承来的同一个，
    //     在这里设与在中间进程设等价，放这里少一次分支。
    if !p.hostname.is_empty() {
        libc::sethostname(p.hostname.as_ptr() as *const libc::c_char, p.hostname.len());
    }

    // 4b. 新 netns 里的 loopback 默认 DOWN，拉起来（见 bring_up_loopback）。
    if p.new_netns {
        bring_up_loopback();
    }

    // 5. 把根挂载点改成 private，否则后续挂载会传播回宿主。
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

    // 5a. overlay 可写层（F9.12）：挂在 rootfs 路径自身之上，之后所有针对
    //     rootfs 的路径（卷挂载目标、设备 bind、pivot_root）看到的都是合并
    //     视图。必须在卷挂载**之前**——反过来卷会被 overlay 盖住。
    //     探测已在父进程做过，这里失败就是真失败，不再回退。
    if let Some(r) = p.new_root.as_ref() {
        if let Some(ov) = r.overlay.as_ref() {
            if libc::mount(
                ov.c_overlay.as_ptr(),
                r.rootfs.as_ptr(),
                ov.c_overlay.as_ptr(),
                0,
                ov.opts.as_ptr() as *const libc::c_void,
            ) != 0
            {
                return Err(err());
            }
        }
    }

    // 5b. 卷挂载（PRD F9.1）。必须在 pivot_root **之前**：切根后旧根被 detach，
    //     宿主源路径就没了。目标已在 fork 前解析成宿主视角路径，这里只做 mount。
    //     两种模式都执行——宿主模式虽不换根，但已在独立 mount namespace 里，
    //     bind 只对容器可见。
    for (src, dst, read_only) in &p.vol_binds {
        if libc::mount(
            src.as_ptr(),
            dst.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null(),
        ) != 0
        {
            return Err(err());
        }
        if *read_only {
            // 只读要**第二次** remount 才生效：首次 bind 会忽略 MS_RDONLY，
            // 这是 mount(2) 的既定行为。少了这步 :ro 会静默变成可写——
            // 那比不支持只读更糟。
            if libc::mount(
                std::ptr::null(),
                dst.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                std::ptr::null(),
            ) != 0
            {
                return Err(err());
            }
        }
    }

    // 宿主程序模式（LinuxMode::Host）：不换根，到此为止。
    let Some(r) = p.new_root.as_ref() else {
        return Ok(());
    };

    // 6. pivot_root 要求新根是一个挂载点。挂了 overlay 时它已经是；
    //    否则把 rootfs bind 到自身。
    if r.overlay.is_none()
        && libc::mount(
            r.rootfs.as_ptr(),
            r.rootfs.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null(),
        ) != 0
    {
        return Err(err());
    }
    // 6b. 最小设备集：逐个 bind 宿主设备节点到 rootfs/dev/*。
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
    // 7. 切根。put_old 必须在新根之内。
    if libc::chdir(r.rootfs.as_ptr()) != 0 {
        return Err(err());
    }
    if libc::syscall(libc::SYS_pivot_root, r.rootfs.as_ptr(), r.put_old.as_ptr()) != 0 {
        return Err(err());
    }
    if libc::chdir(p.c_root.as_ptr()) != 0 {
        return Err(err());
    }
    // 容器内的工作目录（`-w` 或镜像 config 的 WorkingDir）。
    // **失败要让容器起不来**：静默留在 `/` 会让 `WORKDIR /app` 的镜像看着跑起来了，
    // 其实所有相对路径都指错了地方——那种错比起不来难查得多。
    if let Some(w) = &p.c_guest_workdir {
        // 不存在就逐级建出来（docker 同此行为）。已存在返回 EEXIST，忽略即可；
        // 真正建不出来时下面的 chdir 会失败，容器起不来——那是对的：
        // 静默留在 `/` 会让 `WORKDIR /app` 的镜像看着跑起来了，其实所有相对路径
        // 都指错了地方，那种错比起不来难查得多。
        for parent in &p.c_guest_workdir_parents {
            libc::mkdir(parent.as_ptr(), 0o755);
        }
        if libc::chdir(w.as_ptr()) != 0 {
            return Err(err());
        }
    }
    // 8. 挂 /proc（新 PID namespace 的视图；ps/top 之类依赖它）。
    //    rootfs 里没有 /proc 目录时静默跳过——镜像不含它属正常情况，
    //    不该因此让整个容器起不来。
    libc::mount(
        r.c_proc_fstype.as_ptr(),
        r.c_proc.as_ptr(),
        r.c_proc_fstype.as_ptr(),
        0,
        std::ptr::null(),
    );
    // 9. 卸掉旧根：不 detach 的话宿主整个文件系统仍在容器内可见，隔离就是假的。
    if libc::umount2(r.c_put_old_in_new.as_ptr(), libc::MNT_DETACH) != 0 {
        return Err(err());
    }
    libc::rmdir(r.c_put_old_in_new.as_ptr());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 限额方案：两组测试各管一件事 ----
    // A) 注入 `None` 的确定性测试：钉死"没有 cgroup v2 时"的兜底逻辑，
    //    结果与跑测机器的 cgroup 版本无关。
    // B) 真机不变量测试：跑当前宿主的真实路径，只断言"结局必须落在可接受
    //    集合里"。缺了 B，像"这台机器上 --memory 会硬报错"这种缺陷没人发现；
    //    缺了 A，兜底数值就没有确定性覆盖。两组都要。

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
