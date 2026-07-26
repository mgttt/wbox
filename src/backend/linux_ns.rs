//! Linux rootless 隔离原语（`LinuxNativeBackend` 的 L1 实现）。
//!
//! 对应 `docs-architecture.md` §10.5 的 L1：user + mount + pid namespace、
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
    /// rootfs 的宿主绝对路径
    rootfs: CString,
    /// rootfs 内用于暂存旧根的目录（pivot_root 的 put_old）
    put_old: CString,
    /// `/proc/self/setgroups` / `uid_map` / `gid_map`
    p_setgroups: CString,
    p_uid_map: CString,
    p_gid_map: CString,
    uid_map: Vec<u8>,
    gid_map: Vec<u8>,
    /// 容器内的 /proc 挂载点与 put_old 卸载路径（pivot_root 之后的视角）
    c_proc: CString,
    c_put_old_in_new: CString,
    c_proc_fstype: CString,
    c_root: CString,
    c_empty: CString,
    /// 最小设备集的 (宿主源, rootfs 内目标) 绑定对。**必须在 pivot_root 之前
    /// 完成**：切根后旧根已被 detach，宿主的 /dev/* 再也取不到。
    /// user namespace 里 mknod 不被允许，故只能 bind 宿主已有的设备节点。
    dev_binds: Vec<(CString, CString)>,
    /// 资源限额落地方式（cgroup v2 或 rlimit 兜底）
    limits: LimitPlan,
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

/// 探测可写的 cgroup v2 目录。返回 `Some(自己的 cgroup 目录)`。
///
/// 判据：`/proc/self/cgroup` 有 `0::` 行（v2 统一层级）、且
/// `/sys/fs/cgroup/<该路径>` 下能创建子目录（委派已开）。两者缺一即 None——
/// 例如本仓开发容器就是 cgroup **v1**，探测会返回 None 并走 rlimit 兜底。
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
pub(super) fn spawn_isolated(spec: &RunSpec, prepared: &Prepared) -> Result<u32> {
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

    // rootless 映射：把宿主当前 uid/gid 映射成容器内的 root。
    // 写 gid_map 之前必须先写 setgroups=deny，否则内核拒绝（CVE-2014-8989 之后的加固）。
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

    let limits = build_limit_plan(spec)?;
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let plan = NsPlan {
        rootfs: cstr(&rootfs)?,
        put_old: cstr(&put_old_host.to_string_lossy())?,
        p_setgroups: cstr("/proc/self/setgroups")?,
        p_uid_map: cstr("/proc/self/uid_map")?,
        p_gid_map: cstr("/proc/self/gid_map")?,
        uid_map: format!("0 {} 1\n", uid).into_bytes(),
        gid_map: format!("0 {} 1\n", gid).into_bytes(),
        c_proc: cstr("/proc")?,
        c_put_old_in_new: cstr("/.wbox_oldroot")?,
        c_proc_fstype: cstr("proc")?,
        c_root: cstr("/")?,
        c_empty: cstr("")?,
        dev_binds,
        limits,
    };

    if spec.verbose {
        verbose_kv("隔离", "user+mount+pid namespace（rootless）");
        verbose_kv("uid 映射", format!("容器 0 ← 宿主 {}", uid));
        verbose_kv("新根", &rootfs);
    }

    let mut cmd = Command::new(&prepared.cmd[0]);
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
            "启动容器进程 '{}' 失败：{}",
            prepared.cmd[0], e
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

/// pre_exec 闭包主体：建立命名空间并切根。返回 `Err` 会让 spawn 失败。
///
/// # Safety
/// 仅在 fork 后的子进程中调用；内部只做裸 syscall。
unsafe fn enter_namespaces(p: &NsPlan) -> std::io::Result<()> {
    let err = || std::io::Error::last_os_error();

    // 0. 限额必须在 unshare(CLONE_NEWUSER) **之前**落实：进了新 user
    //    namespace 后，宿主 cgroup 文件的写入会因 uid 映射被拒。
    apply_limits(p)?;

    // 1. 建 user + mount + pid namespace。user namespace 让非 root 也能
    //    获得新 mount namespace 内的 CAP_SYS_ADMIN（rootless 的关键）。
    if libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS | libc::CLONE_NEWPID) != 0 {
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

    // 4. 把根挂载点改成 private，否则后续挂载会传播回宿主。
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
    // 5. pivot_root 要求新根是一个挂载点，故把 rootfs bind 到自身。
    if libc::mount(
        p.rootfs.as_ptr(),
        p.rootfs.as_ptr(),
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
    for (src, dst) in &p.dev_binds {
        libc::mount(
            src.as_ptr(),
            dst.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        );
    }
    // 6. 切根。put_old 必须在新根之内。
    if libc::chdir(p.rootfs.as_ptr()) != 0 {
        return Err(err());
    }
    if libc::syscall(libc::SYS_pivot_root, p.rootfs.as_ptr(), p.put_old.as_ptr()) != 0 {
        return Err(err());
    }
    if libc::chdir(p.c_root.as_ptr()) != 0 {
        return Err(err());
    }
    // 7. 挂 /proc（新 PID namespace 的视图；ps/top 之类依赖它）。
    //    rootfs 里没有 /proc 目录时静默跳过——镜像不含它属正常情况，
    //    不该因此让整个容器起不来。
    libc::mount(
        p.c_proc_fstype.as_ptr(),
        p.c_proc.as_ptr(),
        p.c_proc_fstype.as_ptr(),
        0,
        std::ptr::null(),
    );
    // 8. 卸掉旧根：不 detach 的话宿主整个文件系统仍在容器内可见，隔离就是假的。
    if libc::umount2(p.c_put_old_in_new.as_ptr(), libc::MNT_DETACH) != 0 {
        return Err(err());
    }
    libc::rmdir(p.c_put_old_in_new.as_ptr());
    Ok(())
}

/// 把 CLI 的 `--memory`/`--cpu-pct`/`--max-procs` 落成本宿主可执行的限额方案。
///
/// 优先 cgroup v2；不可用时退化 setrlimit。**`--cpu-pct` 没有 rlimit 对应物**
/// （RLIMIT_CPU 限的是累计 CPU 秒数，不是占比），故此时明确报错而非静默忽略
/// ——§10.5 语义一致性红线：宁可拒绝执行，也不能让同一条命令在不同宿主上
/// 隔离强度不同。
fn build_limit_plan(spec: &RunSpec) -> Result<LimitPlan> {
    let l = &spec.limits;
    if l.memory_mb == 0 && l.cpu_pct == 0 && l.max_procs == 0 {
        return Ok(LimitPlan::None);
    }
    if let Some(base) = cgroup2_self_dir() {
        let dir = base.join(format!("wbox-{}", std::process::id()));
        // 创建失败（无委派/只读）不致命：落到 rlimit 分支再判断
        if std::fs::create_dir_all(&dir).is_ok() {
            let mut wrote = Vec::new();
            if l.memory_mb > 0 {
                let v = (l.memory_mb * 1024 * 1024).to_string();
                std::fs::write(dir.join("memory.max"), &v)
                    .map_err(|e| cgroup_err("memory.max", &e))?;
                wrote.push(format!("memory.max={}", v));
            }
            if l.max_procs > 0 {
                std::fs::write(dir.join("pids.max"), l.max_procs.to_string())
                    .map_err(|e| cgroup_err("pids.max", &e))?;
                wrote.push(format!("pids.max={}", l.max_procs));
            }
            if l.cpu_pct > 0 {
                // cpu.max 格式："<quota_us> <period_us>"；period 取默认 100ms
                let period = 100_000u64;
                let quota = period * u64::from(l.cpu_pct) / 100;
                std::fs::write(dir.join("cpu.max"), format!("{} {}", quota, period))
                    .map_err(|e| cgroup_err("cpu.max", &e))?;
                wrote.push(format!("cpu.max={}/{}", quota, period));
            }
            if spec.verbose {
                verbose_kv("限额（cgroup v2）", wrote.join(" "));
                verbose_kv("cgroup", dir.display());
            }
            return Ok(LimitPlan::Cgroup {
                procs: cstr(&dir.join("cgroup.procs").to_string_lossy())?,
            });
        }
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
    // 有进程数上限而实际没有，比直接报错危险得多（§10.5 红线）。
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

fn cgroup_err(file: &str, e: &std::io::Error) -> WboxError {
    WboxError::spawn(format!(
        "写 cgroup v2 的 {} 失败：{}（委派是否开启？rootless 下通常需要 systemd 用户会话）",
        file, e
    ))
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

    /// `--cpu-pct` 在无 cgroup v2 的宿主上**必须报错**（§10.5 红线：
    /// 不静默忽略隔离参数）。有 cgroup v2 时则应成功落成 Cgroup 方案。
    /// 两种环境都断言，避免只在一种机器上有效。
    #[test]
    fn cpu_pct_requires_cgroup2_or_errors() {
        let s = spec_with(Limits { memory_mb: 0, cpu_pct: 50, max_procs: 0 });
        match (cgroup2_self_dir(), build_limit_plan(&s)) {
            (Some(_), Ok(LimitPlan::Cgroup { .. })) => {}
            (Some(_), other) => panic!("有 cgroup v2 却未落成 Cgroup 方案：{:?}", other.is_ok()),
            (None, Err(e)) => {
                let m = format!("{}", e);
                assert!(m.contains("cpu-pct"), "错误应点明是 --cpu-pct：{}", m);
            }
            (None, Ok(_)) => panic!("无 cgroup v2 时 --cpu-pct 必须报错，不得静默忽略"),
        }
    }

    /// 仅内存/进程数时，无 cgroup v2 应退化为 rlimit（而非报错）——
    /// 这两项有可用的 setrlimit 对应物，拒绝执行就过度了。
    #[test]
    fn memory_and_procs_fall_back_to_rlimit() {
        let s = spec_with(Limits { memory_mb: 16, cpu_pct: 0, max_procs: 8 });
        match (cgroup2_self_dir(), build_limit_plan(&s).unwrap()) {
            (Some(_), LimitPlan::Cgroup { .. }) => {}
            (None, LimitPlan::Rlimit { as_bytes, nproc }) => {
                assert_eq!(as_bytes, Some(16 * 1024 * 1024));
                assert_eq!(nproc, Some(8));
            }
            (_, other) => panic!("落成的方案与宿主能力不匹配：{}", match other {
                LimitPlan::None => "None",
                LimitPlan::Cgroup { .. } => "Cgroup",
                LimitPlan::Rlimit { .. } => "Rlimit",
            }),
        }
    }
}
