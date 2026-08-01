//! `wbox stats`：容器的实时资源占用（`PRD.md` F9.24）。
//!
//! # 数据从哪来，为什么是两条路
//!
//! 容器的 cgroup **只在设了限额时才存在**（见 `linux_limits.rs`：没有
//! `--memory`/`--cpu-pct`/`--max-procs` 就不建组）。所以不能像 docker 那样
//! 一律读 cgroup——那会让 `stats` 对一半的容器直接没有答案。
//!
//! 两条路，按可得性挑：
//!
//! - **有专属 cgroup**：读 `memory.current` / `cpu.stat` / `pids.current`。
//!   这是内核自己的记账，精确。
//! - **没有**：按 `top` 的同一份进程枚举去 `/proc` 里逐个加。够用，但内存是
//!   **RSS 合计**，多个进程共享的页会被重复计入——这一点必须在输出里说明，
//!   不能让人把它当成 cgroup 那种精确值。
//!
//! 输出里标出用的是哪条路（`来源` 列）。把两种精度不同的数字印成一个样子，
//! 是在诱导误读。
//!
//! # CPU% 必须采两次
//!
//! `/proc` 与 `cpu.stat` 给的都是**累计**用时。单次采样只能得出"这个容器从
//! 启动到现在平均用了多少 CPU"，那不是 `stats` 要回答的问题。所以隔一个采样
//! 窗口取两次差值——窗口用真实经过的时间算，不假设 sleep 精确。

use crate::error::{Result, WboxError};
#[cfg(target_os = "linux")]
use crate::runstate::{self, Liveness};

// 这个模块除了 `cmd_stats` 的拒绝分支之外整体只在 Linux 有意义：数据来源是
// cgroup 与 /proc，Windows 上一条也不存在。逐项标 cfg 而不是给整个文件加
// `allow(dead_code)`——后者会连真正的死代码一起放过。

/// 采样窗口。太短则 CPU% 抖动大，太长则命令显得卡住。
#[cfg(target_os = "linux")]
const WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct Options<'a> {
    names: Vec<&'a str>,
}

#[cfg(target_os = "linux")]
fn parse<'a>(args: &'a [String]) -> Result<Options<'a>> {
    let mut names = Vec::new();
    for a in args {
        if a.starts_with('-') {
            return Err(WboxError::args(format!(
                "stats: 未知参数 '{}'（用法：wbox stats [NAME...]；不带名字则列出全部运行中的容器）",
                a
            )));
        }
        names.push(a.as_str());
    }
    Ok(Options { names })
}

#[cfg(target_os = "linux")]
/// 一次采样。`cpu_ns` 是累计 CPU 用时（纳秒），`mem` 字节，`pids` 进程数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub cpu_ns: u64,
    pub mem: u64,
    pub pids: u32,
}

#[cfg(target_os = "linux")]
/// 数字是从哪儿来的。决定了内存那一列该怎么读。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// 内核 cgroup 记账，精确。
    Cgroup,
    /// `/proc` 逐进程累加；内存是 RSS 合计，共享页重复计入。
    Proc,
}

#[cfg(target_os = "linux")]
impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::Cgroup => "cgroup",
            Source::Proc => "proc*",
        }
    }
}

#[cfg(target_os = "linux")]
/// 把两次采样折算成 CPU 百分比。
///
/// 分母是**真实经过的时间**，不是标称的采样窗口：sleep 不精确，用标称值会让
/// 结果系统性偏高或偏低。上限不封在 100——多核容器用满两个核就该显示 200%，
/// 与 docker 一致。
pub fn cpu_percent(before: &Sample, after: &Sample, elapsed: std::time::Duration) -> f64 {
    let dt = elapsed.as_nanos();
    if dt == 0 {
        return 0.0;
    }
    let dc = after.cpu_ns.saturating_sub(before.cpu_ns) as f64;
    dc / dt as f64 * 100.0
}

/// 人读的字节数。`stats` 的用途是"扫一眼有没有异常"，给到小数点后一位足够，
/// 精确到字节反而更难扫。
///
/// **不加平台门**：它是纯格式化，没有任何平台依赖，而且 `volume ls` 也要用
/// （F9.35）。早先给它加 `cfg(linux)` 只是因为当时 Windows 上没人调用——
/// 那种"为消死代码警告而加的门"一旦有了新调用者就该撤掉，否则会逼着调用方
/// 各写一份格式化。
pub fn human_bytes(v: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut f = v as f64;
    let mut i = 0;
    while f >= 1024.0 && i + 1 < UNITS.len() {
        f /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{}{}", v, UNITS[0])
    } else {
        format!("{:.1}{}", f, UNITS[i])
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{Sample, Source};

    /// 容器**专属** cgroup 的目录；没有专属组时返回 `None`。
    ///
    /// 判据是"它和 wbox 自己的组不同"：没设限额时容器就待在 wbox 所在的组里，
    /// 那个组的数字含 wbox supervisor 乃至整个会话，拿它当容器占用是错的。
    fn dedicated_cgroup(pid: u32) -> Option<std::path::PathBuf> {
        let of = |p: &str| -> Option<String> {
            let text = std::fs::read_to_string(p).ok()?;
            // cgroup v2 只有一行，形如 `0::/user.slice/...`
            text.lines()
                .find_map(|l| l.strip_prefix("0::").map(|s| s.to_string()))
        };
        let mine = of(&format!("/proc/{}/cgroup", pid))?;
        let ours = of("/proc/self/cgroup")?;
        if mine == ours {
            return None;
        }
        let dir = std::path::PathBuf::from("/sys/fs/cgroup").join(mine.trim_start_matches('/'));
        dir.is_dir().then_some(dir)
    }

    fn read_u64(dir: &std::path::Path, file: &str) -> Option<u64> {
        std::fs::read_to_string(dir.join(file))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// `cpu.stat` 里的 `usage_usec`。该控制器未启用时这一行不存在。
    fn cgroup_cpu_ns(dir: &std::path::Path) -> Option<u64> {
        let text = std::fs::read_to_string(dir.join("cpu.stat")).ok()?;
        text.lines()
            .find_map(|l| l.strip_prefix("usage_usec "))
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|us| us * 1_000)
    }

    fn from_cgroup(dir: &std::path::Path) -> Option<Sample> {
        Some(Sample {
            // CPU 控制器可能没启用（只设了 --memory 时）。这时 CPU 累计取不到，
            // 记 0 而不是放弃整条采样——内存和进程数仍然是有效答案。
            cpu_ns: cgroup_cpu_ns(dir).unwrap_or(0),
            mem: read_u64(dir, "memory.current")?,
            pids: read_u64(dir, "pids.current").unwrap_or(0) as u32,
        })
    }

    /// 逐进程累加。内存是 RSS 合计，共享页重复计入（调用方要如实标注来源）。
    fn from_proc(pids: &[u32]) -> Sample {
        let mut cpu_ns = 0u64;
        let mut mem = 0u64;
        let mut alive = 0u32;
        for &pid in pids {
            // 成员选择和聚合属于 wbox；单 PID 宿主计数由共享平台层读取。
            // 进程随时可能退出，读不到就跳过，不让整次采样失败。
            let Ok(metrics) = agenterm_platform::process_metrics::metrics(pid) else {
                continue;
            };
            let process_cpu_ns = u64::try_from(metrics.cpu_time.as_nanos()).unwrap_or(u64::MAX);
            cpu_ns = cpu_ns.saturating_add(process_cpu_ns);
            mem = mem.saturating_add(metrics.resident_bytes);
            alive = alive.saturating_add(1);
        }
        Sample {
            cpu_ns,
            mem,
            pids: alive,
        }
    }

    /// 采一次。`root` 是容器在宿主上的 PID。
    pub(super) fn sample(root: u32) -> (Sample, Source) {
        if let Some(dir) = dedicated_cgroup(root) {
            if let Some(s) = from_cgroup(&dir) {
                return (s, Source::Cgroup);
            }
        }
        (
            from_proc(&crate::cli::top::container_pids(root)),
            Source::Proc,
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// `/proc` 那条路必须对**本进程**给出合理的数字。这条钉住的是字段偏移：
        /// `comm` 含空格时按空格切会整体错位，utime/stime 会读成别的字段。
        #[test]
        fn proc_sampler_reads_this_process() {
            let s = from_proc(&[std::process::id()]);
            assert_eq!(s.pids, 1);
            assert!(s.mem > 0, "本进程的 RSS 不该是 0");
        }

        /// 读不到的 pid 不该让整次采样炸掉——容器里的进程随时会退出。
        #[test]
        fn dead_pids_are_skipped_not_fatal() {
            let s = from_proc(&[u32::MAX, std::process::id()]);
            assert_eq!(s.pids, 1, "只应统计还活着的那个");
        }

        /// 没有专属 cgroup 时必须返回 None。返回 wbox 自己的组会把 supervisor
        /// 乃至整个会话的占用算到容器头上。
        #[test]
        fn own_cgroup_is_not_mistaken_for_a_dedicated_one() {
            assert!(dedicated_cgroup(std::process::id()).is_none());
        }
    }
}

pub fn cmd_stats(args: &[String]) -> Result<u32> {
    // 平台判断在参数解析**之前**：不管参数写得对不对，Windows 上的答案都是
    // "这条命令还没有实现"，先纠参数格式属于答非所问。
    WboxError::require_linux(
        true,
        "stats",
        "占用数据取自 cgroup / /proc，Windows 侧要另走 Job object 的记账接口（尚未实现）",
    )?;
    #[cfg(target_os = "linux")]
    {
        let opts = parse(args)?;
        let names = if opts.names.is_empty() {
            running_containers()
        } else {
            opts.names.iter().map(|s| s.to_string()).collect()
        };
        if names.is_empty() {
            println!("没有运行中的容器");
            return Ok(0);
        }
        // 先把所有容器的第一次采样都取到，再统一等一个窗口——逐个容器各等
        // 500ms 的话，列得越多命令越慢，而且各行的窗口还不重合。
        let mut rows = Vec::new();
        for n in &names {
            let root = resolve_root(n)?;
            let (s, src) = linux::sample(root);
            rows.push((n.clone(), root, s, src));
        }
        let t0 = std::time::Instant::now();
        std::thread::sleep(WINDOW);
        let elapsed = t0.elapsed();

        println!(
            "{:<20} {:>8} {:>12} {:>6}  来源",
            "容器", "CPU %", "内存", "进程"
        );
        let mut proc_used = false;
        for (name, root, before, _) in rows {
            let (after, src) = linux::sample(root);
            proc_used |= src == Source::Proc;
            println!(
                "{:<20} {:>7.2}% {:>12} {:>6}  {}",
                name,
                cpu_percent(&before, &after, elapsed),
                human_bytes(after.mem),
                after.pids,
                src.label()
            );
        }
        if proc_used {
            println!(
                "\n* proc：该容器没有专属 cgroup（未设 --memory/--cpu-pct/--max-procs），\
                 数字按 /proc 逐进程累加；内存为 RSS 合计，进程间共享的页会重复计入。"
            );
        }
        Ok(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        unreachable!("require_linux 已在上面拒绝")
    }
}

/// 解析容器名 → 宿主 PID，并把"没跑起来"的两种情形分开说。
#[cfg(target_os = "linux")]
fn resolve_root(name: &str) -> Result<u32> {
    let dir = runstate::resolve_existing(name)?;
    match runstate::liveness(&dir) {
        Liveness::Running => {}
        Liveness::Created => {
            return Err(WboxError::args(format!(
                "容器 '{}' 尚未启动（状态为 created），没有可采的占用",
                name
            )))
        }
        Liveness::Exited => {
            return Err(WboxError::args(format!(
                "容器 '{}' 已退出，没有可采的占用",
                name
            )))
        }
    }
    runstate::container_pid(&dir).ok_or_else(|| {
        WboxError::args(format!(
            "容器 '{}' 刚启动，尚未记录容器 PID；稍后重试",
            name
        ))
    })
}

/// 不带名字时列哪些容器：**只列运行中的**，与 `wbox ps` 的默认视图一致
/// （已退出的没有可采的占用，列出来只会是一排 0）。
///
/// 枚举复用 `runstate::list`，不另写一遍目录扫描——另写一份的话，
/// 迟早出现"ps 看得见、stats 看不见"这种偏差。
#[cfg(target_os = "linux")]
fn running_containers() -> Vec<String> {
    let Ok(all) = runstate::list() else {
        return Vec::new();
    };
    let mut out: Vec<String> = all
        .into_iter()
        .filter(|(_, live)| *live == Liveness::Running)
        .map(|(e, _)| e.name)
        .collect();
    out.sort();
    out
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn parse_rejects_options_and_takes_names() {
        assert!(parse(&[]).unwrap().names.is_empty());
        assert_eq!(parse(&["a".to_string()]).unwrap().names, vec!["a"]);
        let m = format!("{}", parse(&["--all".to_string()]).unwrap_err());
        assert!(m.contains("NAME"), "要给出用法：{}", m);
    }

    /// CPU% 的分母是**真实经过的时间**。用标称窗口会让结果系统性偏差；
    /// 而且满两个核就该显示 200%，不能封在 100（与 docker 一致）。
    #[test]
    fn cpu_percent_uses_real_elapsed_and_is_not_capped_at_100() {
        let a = Sample {
            cpu_ns: 0,
            mem: 0,
            pids: 1,
        };
        let b = Sample {
            cpu_ns: 500_000_000,
            mem: 0,
            pids: 1,
        };
        let one_sec = std::time::Duration::from_secs(1);
        assert!((cpu_percent(&a, &b, one_sec) - 50.0).abs() < 0.01);
        // 同样的 CPU 增量，窗口只有一半 → 百分比翻倍
        assert!((cpu_percent(&a, &b, one_sec / 2) - 100.0).abs() < 0.01);
        // 两个核跑满
        let c = Sample {
            cpu_ns: 2_000_000_000,
            ..a
        };
        assert!((cpu_percent(&a, &c, one_sec) - 200.0).abs() < 0.01);
        // 零窗口不能除零
        assert_eq!(cpu_percent(&a, &b, std::time::Duration::ZERO), 0.0);
        // 计数器回退（进程换了）不能变成负数
        assert_eq!(cpu_percent(&b, &a, one_sec), 0.0);
    }

    #[test]
    fn human_bytes_switches_units() {
        assert_eq!(human_bytes(512), "512B");
        assert_eq!(human_bytes(1536), "1.5KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0MiB");
    }

    /// 两种来源的精度不同，标签必须不同——印成一个样子是在诱导误读。
    #[test]
    fn sources_are_labelled_distinctly() {
        assert_ne!(Source::Cgroup.label(), Source::Proc.label());
    }

    /// created / exited 要分开说：都笼统报"没有占用"的话，用户分不清是没启动
    /// 还是跑完了。
    #[cfg(target_os = "linux")]
    #[test]
    fn not_running_containers_are_explained_by_state() {
        let _home = TempHome::new("statsstate");
        let dir = runstate::dir_for("c").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let m = format!("{}", resolve_root("c").unwrap_err());
        assert!(m.contains("created") || m.contains("已退出"), "{}", m);
        assert!(resolve_root("nosuch").is_err());
    }

    #[test]
    fn unknown_container_is_an_error() {
        let _home = TempHome::new("statsunknown");
        if cfg!(target_os = "linux") {
            assert!(cmd_stats(&["nope".to_string()]).is_err());
        }
    }
}
