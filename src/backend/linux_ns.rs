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
