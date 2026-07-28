//! Linux 资源限额的**规划与落实**（`PRD.md` F5.6/F5.7）。
//!
//! 从 `linux_ns.rs` 拆出来：那边是 namespace / 换根 / 进程生命周期这类
//! "把进程关进盒子"的原语，这边是"盒子多大"。两件事的唯一接口是
//! [`LimitPlan`]——`NsPlan` 持有它，`pre_exec` 里调 [`apply_limits`] 落实。
//! 合在一个文件里时该文件超过 1100 行，两个主题交错，改一个要跳过另一个。
//!
//! # 为什么"规划"和"落实"分成两步
//!
//! 规划（`build_limit_plan`）在 **fork 之前**做完：要读 `/proc`、建目录、
//! 写 cgroup 文件，全是可能失败、需要报错、需要分配内存的操作。
//! 落实（`apply_limits`）在 **fork 之后、exec 之前**，受 async-signal-safety
//! 约束，只能做裸 syscall。把两者分开，错误处理才能留在能好好报错的一侧。

use super::super::{verbose_kv, RunSpec};
use super::ns::{cstr, write_proc_self};
use crate::error::{Result, WboxError};
use std::ffi::CString;

/// 资源限额的落地方式。cgroup v2 是首选（语义与 Windows 侧 Job Object
/// 最接近：内存/CPU/进程数都是"整组"上限）；不可用时退化为 setrlimit，
/// 但**只有部分语义能对上**，差异见各字段注释。
pub(super) enum LimitPlan {
    /// cgroup v2：guest 要加入的 `cgroup.procs` 路径，外加收尾时要删除的
    /// 目录清单（guest target 与 wbox 自己的 supervisor leaf）。
    Cgroup {
        procs: CString,
        cleanup: Vec<std::path::PathBuf>,
    },
    /// 无 cgroup v2：用 setrlimit 兜底。
    /// - `as_bytes`：RLIMIT_AS ≈ `--memory`。**语义差异**：Job Object 与
    ///   cgroup 限的是实际占用（RSS/page charge），RLIMIT_AS 限的是虚拟地址
    ///   空间总量，对大量 reserve-but-not-commit 的程序会偏严。
    /// - `nproc`：RLIMIT_NPROC ≈ `--max-procs`。**语义差异**：它是
    ///   *按 uid 计* 的全局进程数，不是本容器的；rootless 下容器内 uid 被
    ///   映射成 0，实际约束的是该映射 uid 的总进程数，仍能挡住 fork 炸弹。
    Rlimit {
        as_bytes: Option<u64>,
        nproc: Option<u64>,
    },
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
/// cgroup v2 统一层级的挂载点。
const CGROUP2_ROOT: &str = "/sys/fs/cgroup";

fn cgroup2_self_dir() -> Option<std::path::PathBuf> {
    let content = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let rel = content
        .lines()
        .find_map(|l| l.strip_prefix("0::"))?
        .trim_start_matches('/')
        .to_string();
    let dir = std::path::Path::new(CGROUP2_ROOT).join(&rel);
    // 必须确认 v2 特征文件存在，否则可能是 v1 的 tmpfs
    if !dir.join("cgroup.controllers").is_file() {
        return None;
    }
    Some(dir)
}

/// 把 CLI 的 `--memory`/`--cpu-pct`/`--max-procs` 落成本宿主可执行的限额方案。
///
/// 优先 cgroup v2；不可用时退化 setrlimit。**`--cpu-pct` 没有 rlimit 对应物**
/// （RLIMIT_CPU 限的是累计 CPU 秒数，不是占比），故此时明确报错而非静默忽略
/// ——PRD F5 一致性要求：宁可拒绝执行，也不能让同一条命令在不同宿主上
/// 隔离强度不同。
pub(super) fn build_limit_plan(spec: &RunSpec) -> Result<LimitPlan> {
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
/// 策略 A：把限额 target 建成 `own` 的**兄弟**（挂在 `own` 的父级下）。
///
/// 这是最省事的形态——**谁都不用挪进程**，因此不受"own 里还有别的进程"影响
/// （从 shell 启动 wbox 时，shell 就留在 own 里，策略 B 会因此失败）。
/// 代价是需要对 `own` 的父级有写权限；委派根把子树交给我们时通常满足，
/// 不满足就返回 `None` 让调用方退到策略 B。
fn try_sibling_layout(spec: &RunSpec, own: &std::path::Path) -> Result<Option<LimitPlan>> {
    let Some(parent) = own.parent() else {
        return Ok(None);
    };
    // **绝不动根 cgroup。** 往 /sys/fs/cgroup/cgroup.subtree_control 写是
    // 整机范围的改动（会给所有顶层 cgroup 打开该控制器的记账），一个
    // "把自己关进沙箱"的工具没有理由去改宿主的全局设置——哪怕权限允许。
    // 这种情形退回策略 B，或者干脆用 rlimit 兜底。
    if parent == std::path::Path::new(CGROUP2_ROOT) {
        return Ok(None);
    }
    // 不越过 cgroup v2 挂载点：/sys/fs/cgroup 之上不是我们该碰的地方
    if !parent.join("cgroup.controllers").is_file() {
        return Ok(None);
    }
    let target = parent.join(format!("wbox-{}", std::process::id()));
    if std::fs::create_dir_all(&target).is_err() {
        return Ok(None); // 父级不可写：交给策略 B
    }
    let cleanup_on_fail = || {
        let _ = std::fs::remove_dir(&target);
        Ok(None)
    };
    let want = needed_controllers(&spec.limits);
    if !want.is_empty()
        && std::fs::write(parent.join("cgroup.subtree_control"), want.join(" ")).is_err()
    {
        return cleanup_on_fail();
    }
    match write_limits(spec, &target) {
        Some(wrote) => {
            if spec.verbose {
                verbose_kv("限额（cgroup v2）", wrote.join(" "));
                verbose_kv("cgroup（guest）", target.display());
                verbose_kv("cgroup 布局", "兄弟位置（wbox 自身不迁移）");
            }
            Ok(Some(LimitPlan::Cgroup {
                procs: cstr(&target.join("cgroup.procs").to_string_lossy())?,
                cleanup: vec![target],
            }))
        }
        None => cleanup_on_fail(),
    }
}

/// 往 target 写各项限额。全成功返回写了什么（供 -V 打印），任一失败返回 None。
fn write_limits(spec: &RunSpec, target: &std::path::Path) -> Option<Vec<String>> {
    let l = &spec.limits;
    let mut wrote = Vec::new();
    if l.memory_mb > 0 {
        let v = (l.memory_mb * 1024 * 1024).to_string();
        std::fs::write(target.join("memory.max"), &v).ok()?;
        wrote.push(format!("memory.max={}", v));
        // **必须同时封掉 swap**，否则 `--memory` 在两条路径上的含义不一样：
        // `RLIMIT_AS` 直接让分配失败，而 `memory.max` 只限**常驻内存**，
        // 超出的页会被换出去，程序照跑不误。门禁实测抓到过：同一条
        // `--memory 16`，rlimit 路径下 64MB 分配失败，cgroup 路径下却成功
        // （runner 开着 swap）。同一条命令在不同宿主上强度不同，正是
        // PRD F5 一致性要求禁止的。
        //
        // 文件不存在 = 内核没开 swap 记账 = 本来就没有 swap 逃逸，跳过即可；
        // 存在却写不进去则放弃 cgroup 方案——宁可退回 rlimit，也不要一个
        // 名不副实的"内存上限"。
        let swap_max = target.join("memory.swap.max");
        if swap_max.exists() {
            std::fs::write(&swap_max, "0").ok()?;
            wrote.push("memory.swap.max=0".to_string());
        }
    }
    if l.max_procs > 0 {
        std::fs::write(target.join("pids.max"), l.max_procs.to_string()).ok()?;
        wrote.push(format!("pids.max={}", l.max_procs));
    }
    if l.cpu_pct > 0 {
        // cpu.max 格式："<quota_us> <period_us>"；period 取默认 100ms
        let period = 100_000u64;
        let quota = period * u64::from(l.cpu_pct) / 100;
        std::fs::write(target.join("cpu.max"), format!("{} {}", quota, period)).ok()?;
        wrote.push(format!("cpu.max={}/{}", quota, period));
    }
    Some(wrote)
}

fn try_cgroup_plan(spec: &RunSpec, base: Option<std::path::PathBuf>) -> Result<Option<LimitPlan>> {
    let l = &spec.limits;
    let Some(own) = base else {
        return Ok(None);
    };

    // ---- 布局（实机取证过，见 docs/architecture.md §3.3）----
    // 核心约束：写限额的那个 cgroup，其**父级**必须已 enable subtree_control，
    // 而 enable 的前提是该父级**没有直接进程**。
    //
    // 策略 A（首选）：把 target 建成 own 的**兄弟**（挂在 own 的父级下）。
    //   parent/
    //     ├── own/          wbox 和它的调用者都在这儿，不用动
    //     └── wbox-<pid>/   限额写这里
    //   好处是**谁都不用挪**。这条能成立的前提是 parent 可写且已下发控制器
    //   —— 委派根把子树交给我们时通常就是这个形状。
    //
    // 策略 B（兜底）：parent 不可写时，退回"在 own 内部自建两个 leaf"：
    //   own/
    //     ├── wbox-supervisor/   wbox 把自己挪进来
    //     └── wbox-<pid>/        限额写这里
    //   代价是要求 own 里**只有 wbox 自己**——否则挪走 wbox 之后 own 仍有
    //   别的进程（典型情况：从 shell 里启动 wbox，shell 留在同一个 cgroup），
    //   enable 依旧 EBUSY。实测正是这么暴露出来的：探针里 shell `exec` 成了
    //   wbox 故只有一个进程，能过；而门禁里脚本 shell 还在，就过不了。
    //
    // 无论如何都不能用旧布局（own/wbox-<pid> 且 wbox 留在 own）：那要求对
    // "自己正待着的 cgroup" enable subtree_control，内核 EBUSY；反过来先
    // enable 再塞进程则 EIO。两个方向都堵死。
    if let Some(plan) = try_sibling_layout(spec, &own)? {
        return Ok(Some(plan));
    }
    let supervisor = own.join("wbox-supervisor");
    let target = own.join(format!("wbox-{}", std::process::id()));
    if std::fs::create_dir_all(&supervisor).is_err() || std::fs::create_dir_all(&target).is_err() {
        return Ok(None); // 委派未开
    }
    // 清理助手：放弃时把自己建的目录删掉。**成功路径也要清**（见函数尾部
    // 说明），否则每跑一次就在宿主留一个空 cgroup。
    let cleanup = || {
        let _ = std::fs::remove_dir(&target);
        let _ = std::fs::remove_dir(&supervisor);
    };
    let abandon = || {
        cleanup();
        Ok(None)
    };

    // 把**自己**挪进 supervisor：这一步之后 own 才没有直接进程，
    // 才可能给子级下发控制器。挪不动就说明这里不是我们能支配的委派根。
    //
    // 注意这是一个**不可回滚的副作用**：一旦挪成功，即便后续步骤失败退回
    // rlimit，wbox 也仍待在 supervisor 里（不再挪回去——挪回 own 会让 own
    // 重新变成"含进程"，反而把状态搞乱）。后果仅是 wbox 自己多待在一层
    // 子 cgroup 中，不影响限额语义；代价是 `wbox-supervisor` 这个目录删不掉
    // （里面有进程），会留在委派根下。它是**固定名字**而非 per-pid，故最多
    // 只留一个，不会累积。
    if std::fs::write(
        supervisor.join("cgroup.procs"),
        std::process::id().to_string(),
    )
    .is_err()
    {
        return abandon();
    }
    // 下发控制器。已经开好了也无妨（重复写 "+memory" 是幂等的）。
    let want = needed_controllers(l);
    if !want.is_empty()
        && std::fs::write(own.join("cgroup.subtree_control"), want.join(" ")).is_err()
    {
        return abandon();
    }

    // 与策略 A 共用同一个写入函数：两条路径的限额语义必须**逐字节一致**，
    // 各写各的迟早会漂（比如只在一边封了 swap）。
    let Some(wrote) = write_limits(spec, &target) else {
        return abandon();
    };
    if spec.verbose {
        verbose_kv("限额（cgroup v2）", wrote.join(" "));
        verbose_kv("cgroup（guest）", target.display());
        verbose_kv("cgroup（wbox 自身）", supervisor.display());
    }
    Ok(Some(LimitPlan::Cgroup {
        procs: cstr(&target.join("cgroup.procs").to_string_lossy())?,
        // 收尾时要删的两个目录。旧实现只在失败路径删，成功路径从不清理——
        // 每成功跑一次就在宿主留一个空的 wbox-<pid>，而 cgroup v2 不会自动回收。
        // 这个泄漏一直没被发现，因为成功路径在任何环境都没执行过。
        cleanup: vec![target, supervisor],
    }))
}

/// 本次请求需要下发哪些控制器。只开用得上的，少动宿主状态。
fn needed_controllers(l: &crate::backend::Limits) -> Vec<&'static str> {
    let mut v = Vec::new();
    if l.memory_mb > 0 {
        v.push("+memory");
    }
    if l.max_procs > 0 {
        v.push("+pids");
    }
    if l.cpu_pct > 0 {
        v.push("+cpu");
    }
    v
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
pub(super) unsafe fn apply_limits(plan: &LimitPlan) -> std::io::Result<()> {
    match plan {
        LimitPlan::None => Ok(()),
        LimitPlan::Cgroup { procs, .. } => {
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
                let rl = libc::rlimit {
                    rlim_cur: *v,
                    rlim_max: *v,
                };
                if libc::setrlimit(libc::RLIMIT_AS, &rl) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            if let Some(v) = nproc {
                let rl = libc::rlimit {
                    rlim_cur: *v,
                    rlim_max: *v,
                };
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
            name: "t".to_string(),
            limits,
            workdir: std::env::temp_dir(),
            cmd: vec!["/bin/true".to_string()],
            ..RunSpec::default()
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

    #[test]
    fn cpu_pct_without_cgroup_errors() {
        let s = spec_with(Limits {
            memory_mb: 0,
            cpu_pct: 50,
            max_procs: 0,
        });
        let e = match build_limit_plan_with_cgroup(&s, None) {
            Err(e) => e,
            Ok(_) => panic!("无 cgroup 时 --cpu-pct 必须报错"),
        };
        let m = format!("{}", e);
        assert!(m.contains("cpu-pct"), "错误应点明是 --cpu-pct：{}", m);
    }

    #[test]
    fn memory_falls_back_to_rlimit() {
        let s = spec_with(Limits {
            memory_mb: 16,
            cpu_pct: 0,
            max_procs: 0,
        });
        match build_limit_plan_with_cgroup(&s, None).unwrap() {
            LimitPlan::Rlimit { as_bytes, nproc } => {
                assert_eq!(as_bytes, Some(16 * 1024 * 1024));
                assert_eq!(nproc, None);
            }
            _ => panic!("无 cgroup 时内存限额应落成 Rlimit"),
        }
    }

    #[test]
    fn max_procs_fallback_matches_privilege() {
        let s = spec_with(Limits {
            memory_mb: 0,
            cpu_pct: 0,
            max_procs: 8,
        });
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

    #[test]
    fn cpu_pct_on_this_host_is_cgroup_or_error() {
        let s = spec_with(Limits {
            memory_mb: 0,
            cpu_pct: 50,
            max_procs: 0,
        });
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

    #[test]
    fn memory_and_procs_on_this_host_never_silently_ignored() {
        let s = spec_with(Limits {
            memory_mb: 16,
            cpu_pct: 0,
            max_procs: 8,
        });
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

    #[test]
    fn sibling_layout_never_touches_root_cgroup() {
        let s = spec_with(Limits {
            memory_mb: 16,
            cpu_pct: 0,
            max_procs: 0,
        });
        // own 是根的直接子级 → parent 就是根 → 必须拒绝（返回 None 走兜底）
        let own = std::path::Path::new(CGROUP2_ROOT).join("some-toplevel-cg");
        let got = try_sibling_layout(&s, &own).unwrap();
        assert!(
            got.is_none(),
            "own 的父级是根 cgroup 时，策略 A 必须放弃而不是去写根的 subtree_control"
        );
        // 而且不能在根下留下任何 wbox-* 目录
        let leaked = std::fs::read_dir(CGROUP2_ROOT)
            .map(|d| {
                d.filter_map(|e| e.ok())
                    .any(|e| e.file_name().to_string_lossy().starts_with("wbox-"))
            })
            .unwrap_or(false);
        assert!(!leaked, "策略 A 在根 cgroup 下留下了 wbox-* 目录");
    }

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
}
