//! 运行中容器的状态目录与发现（`PRD.md` F8.a）。
//!
//! 目录布局：`~/.wbox/run/<name>/`
//! - `meta.json`：容器规格摘要（名字、pid、命令行、目标、创建时间）；
//! - `lock`：**由运行中的 wbox 独占持有**的空文件，存活判定全靠它。
//!
//! # 为什么用锁文件而不是 pid
//!
//! pid 会被复用。拿"`/proc/<pid>` 还在"当存活判据，在一台繁忙机器上迟早会把
//! 别人的进程认成自己的容器——而 `stop` 一旦据此发信号，就是杀错进程。
//!
//! 起初打算用 pid + `/proc/<pid>/stat` 的 starttime 做二元组来识破复用
//! （这是常见做法），但它有两个缺点：一是 Linux 专有，Windows 要另写一套
//! `GetProcessTimes`；二是那套 Windows 代码我在本机无从验证。
//!
//! 锁文件同时解决这两点：**两个平台的语义都由操作系统保证**——
//! 进程无论怎么死（正常退出、崩溃、被 SIGKILL），内核都会关掉它的 fd/句柄，
//! 锁随之释放。于是"能拿到锁"精确等价于"没有活着的 owner"，不存在 pid 复用
//! 这类误判，也不需要清理钩子跑到才算数。而且同一套单测能同时覆盖两侧实现
//! （见本文件末尾的 `lock_reflects_owner_liveness`），Windows 那半由 CI 的
//! windows runner 真实执行，不是我拍脑袋写完就算。

use crate::error::{Result, WboxError};
use std::fs::File;
use std::path::{Path, PathBuf};

/// 状态根目录：`~/.wbox/run`（与镜像缓存同在 `~/.wbox` 下，便于统一清理）。
pub fn run_root() -> Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| WboxError::args("无法确定用户主目录（USERPROFILE/HOME 均未设置）"))?;
    Ok(PathBuf::from(home).join(".wbox").join("run"))
}

/// 容器名 → 状态目录。名字已由 `validate_container_name` 校验过长度，
/// 这里再挡一次路径分隔符：`--name ../x` 不能把状态目录写到别处去。
pub fn dir_for(name: &str) -> Result<PathBuf> {
    if name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(WboxError::args(format!(
            "容器名 '{}' 含路径分隔符，不能用作状态目录名",
            name
        )));
    }
    Ok(run_root()?.join(name))
}

/// 状态目录里记录的容器摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// 记录用途：展示与排查。**不用于存活判定**（见模块文档）。
    pub pid: u32,
    /// Unix 秒。不引 chrono——只为了显示"跑了多久"，精度到秒足够。
    pub created_unix: u64,
    /// guest 命令行（已合并 Entrypoint/Cmd）
    pub cmd: Vec<String>,
    /// 运行目标：镜像引用，或宿主程序模式下的 `(native)`
    pub target: String,
}

impl Entry {
    fn to_json(&self) -> String {
        let v = serde_json::json!({
            "name": self.name,
            "pid": self.pid,
            "created_unix": self.created_unix,
            "cmd": self.cmd,
            "target": self.target,
        });
        v.to_string()
    }

    /// 解析 `meta.json`。字段缺失/类型不对一律返回 `None`——状态目录可能被
    /// 用户手改或被上一版 wbox 写过，读不懂就当这条不存在，不要让 `ps` 崩掉。
    fn from_json(text: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        Some(Entry {
            name: v.get("name")?.as_str()?.to_string(),
            pid: u32::try_from(v.get("pid")?.as_u64()?).ok()?,
            created_unix: v.get("created_unix")?.as_u64()?,
            cmd: v
                .get("cmd")?
                .as_array()?
                .iter()
                .map(|x| x.as_str().unwrap_or_default().to_string())
                .collect(),
            target: v.get("target")?.as_str()?.to_string(),
        })
    }
}

/// 容器存活状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// 锁被占住 → 有活着的 owner
    Running,
    /// 锁能拿到 → owner 已消失（正常退出没来得及清理，或崩溃）
    Exited,
}

/// 独占打开一个文件。成功即表示**此刻没有别人持有它**。
///
/// Linux 用 `flock(LOCK_EX|LOCK_NB)`：锁绑定在 *open file description* 上，
/// 因此同一进程再打开一次也会冲突——这正是我们要的（单测就在同进程内验证）。
/// 换成 `fcntl` 记录锁则不行：那是按进程算的，同进程重复加锁会直接成功。
#[cfg(unix)]
fn try_lock_exclusive(path: &Path) -> Option<File> {
    use std::os::unix::io::AsRawFd;
    let f = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .ok()?;
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Some(f)
    } else {
        None
    }
}

/// Windows：`share_mode(0)` 表示打开期间不允许他人共享访问，
/// 别的进程再开就会拿到共享冲突——与 flock 等效。
#[cfg(windows)]
fn try_lock_exclusive(path: &Path) -> Option<File> {
    use std::os::windows::fs::OpenOptionsExt;
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
        .ok()
}

/// 判定一个状态目录的存活状态。
pub fn liveness(dir: &Path) -> Liveness {
    let lock = dir.join("lock");
    if !lock.exists() {
        // 没有锁文件：要么根本没登记完，要么被清理过——都按已亡处理
        return Liveness::Exited;
    }
    match try_lock_exclusive(&lock) {
        // 能独占 → 没有 owner。拿到后立刻释放（drop File）
        Some(_) => Liveness::Exited,
        None => Liveness::Running,
    }
}

/// 一次运行的登记。**RAII**：drop 时释放锁，并按 `persist` 决定是否清目录。
///
/// 崩溃时 drop 不会执行，状态目录会残留——这是刻意接受的：锁已随进程消失，
/// [`liveness`] 会把它判为 `Exited`，`ps` 如实显示，`rm` 负责清理。
/// 与其为"保证清理"引入守护进程，不如让残留可见且可解释。
#[derive(Debug)]
pub struct Registration {
    dir: PathBuf,
    /// 持有到 drop 为止；这个句柄就是"我还活着"的证据
    lock: Option<File>,
    /// 退出后**保留**记录与日志（`--detach` 用）。
    ///
    /// 前台容器退出即清理：输出已经打在用户终端上了，留个空目录只是垃圾。
    /// 后台容器相反——用户回头查 `wbox logs` 正是为了看它到底输出了什么，
    /// 一退出就把日志删掉等于把这个命令的主要用途废掉。因此后台记录保留为
    /// `exited`，由 `wbox rm` 显式清理（与 docker 的 ps -a / rm 一致）。
    persist: bool,
}

impl Registration {
    /// 状态目录路径。目前只有单测用得上（F8.2 的 `logs` 会用它定位日志文件），
    /// 故标 `cfg(test)` 而不是挂 `allow(dead_code)`——后者会连真正的死代码
    /// 一起放过。
    #[cfg(test)]
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// 删掉一个状态目录的全部内容。**调用前必须确认没有 owner 持锁**
/// （Drop 里是自己刚放的手，其余路径靠 [`liveness`] 判定）。
///
/// 尽力而为、不报错：清理失败顶多留下一条 `Exited` 记录，比 panic 或让
/// 调用方多一条错误分支都好。
fn purge_dir(dir: &Path) {
    let _ = std::fs::remove_file(dir.join("meta.json"));
    let _ = std::fs::remove_file(dir.join("lock"));
    // 日志也要删：留着会让 remove_dir 因非空而失败，状态目录就永远清不掉
    let _ = std::fs::remove_file(dir.join(LOG_STDOUT));
    let _ = std::fs::remove_file(dir.join(LOG_STDERR));
    let _ = std::fs::remove_dir(dir);
}

impl Drop for Registration {
    fn drop(&mut self) {
        // 先放锁：无论留不留目录，都要让 liveness 立刻能判出"已退出"。
        // （Windows 还额外要求关掉句柄才能删文件。）
        drop(self.lock.take());
        if !self.persist {
            purge_dir(&self.dir);
        }
    }
}

/// 日志文件名（`--detach` 时 guest 的 stdout/stderr 落盘处）。
pub const LOG_STDOUT: &str = "stdout.log";
pub const LOG_STDERR: &str = "stderr.log";

/// 日志体积上限。超过后**丢旧留新**并在截断处留标记——见 [`enforce_log_cap`]。
/// 默认 8 MiB；`WBOX_LOG_MAX_BYTES` 可覆盖（测试用小值，不必写满 8MB）。
pub fn log_cap_bytes() -> u64 {
    std::env::var("WBOX_LOG_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(8 * 1024 * 1024)
}

/// 以**追加**方式打开日志文件。追加是关键：[`enforce_log_cap`] 靠截断控制
/// 体积，而只有 append 模式的写入才会在截断后从新的文件末尾（0）继续；
/// 普通写模式下 writer 自带偏移，截断后文件立刻变回原大小的稀疏文件，
/// 上限就形同虚设。
pub fn open_log_append(dir: &Path, file: &str) -> Result<File> {
    File::options()
        .create(true)
        .append(true)
        .open(dir.join(file))
        .map_err(|e| WboxError::args(format!("打开日志 '{}' 失败：{}", file, e)))
}

/// 超上限则清空并写入一行截断说明。
///
/// 这是**丢旧留新**的粗粒度轮转，不是精细的按行轮转：容器日志的价值绝大多数
/// 在最近的输出，而实现精确轮转要在 wbox 与 guest 之间插一层转写，代价远大于
/// 收益。关键是两条：体积有界，且**截断这件事对用户可见**——无声丢日志比
/// 日志不全更糟。
pub fn enforce_log_cap(dir: &Path, file: &str) {
    let path = dir.join(file);
    let Ok(md) = std::fs::metadata(&path) else {
        return;
    };
    let cap = log_cap_bytes();
    if md.len() <= cap {
        return;
    }
    // 截断到 0；append 写入方随后从头继续（见 open_log_append 的说明）
    if let Ok(f) = File::options().write(true).open(&path) {
        let _ = f.set_len(0);
        use std::io::Write;
        let mut f = f;
        let _ = writeln!(
            f,
            "[wbox] 日志超过 {} 字节上限，已丢弃此前内容（保留最新输出）",
            cap
        );
    }
}

/// [`register_with`] 的简写（不保留日志）。生产路径一律走 `register_with`
/// ——它要按是否 supervised 决定日志去留——故此处只剩测试在用。
#[cfg(test)]
pub fn register(name: &str, cmd: &[String], target: &str) -> Result<Registration> {
    register_with(name, cmd, target, false)
}

/// [`register`] 的完整形式。`keep_logs=true` 时不清除既有日志——
/// `--detach` 流程里日志文件是**父进程**先建好、再作为子进程 stdio 传下去的，
/// 子进程随后才登记；此时若按"复用残留"的常规逻辑清目录，就会把父进程刚建好
/// 的日志一起删掉，容器还没开口就先失声。
pub fn register_with(
    name: &str,
    cmd: &[String],
    target: &str,
    keep_logs: bool,
) -> Result<Registration> {
    let dir = dir_for(name)?;
    if dir.exists() && keep_logs {
        // detach 的子进程：目录是父进程刚建的，锁还没人持有，直接接手即可
        let lock = try_lock_exclusive(&dir.join("lock")).ok_or_else(|| {
            WboxError::args(format!("无法独占容器 '{}' 的锁文件（并发的 wbox？）", name))
        })?;
        write_meta(&dir, name, cmd, target)?;
        return Ok(Registration {
            dir,
            lock: Some(lock),
            // detach：退出后保留，供 `wbox logs` 事后查看
            persist: true,
        });
    }
    if dir.exists() {
        match liveness(&dir) {
            Liveness::Running => {
                return Err(WboxError::args(format!(
                    // 别在这里建议 `wbox rm`：已退出的残留在上面就被自动复用了，
                    // 所以能走到这条分支的**一定是运行中的**，而 rm 明确拒绝
                    // 运行中的容器——那样等于指使用户去撞一堵墙。
                    "容器名 '{}' 正在使用中。换个 --name，或先停掉正在运行的那个",
                    name
                )))
            }
            // 已亡的残留：可以安全复用，直接清掉重来
            Liveness::Exited => purge_dir(&dir),
        }
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| WboxError::args(format!("创建状态目录 '{}' 失败：{}", dir.display(), e)))?;
    let lock = try_lock_exclusive(&dir.join("lock")).ok_or_else(|| {
        WboxError::args(format!(
            "无法独占容器 '{}' 的锁文件（是否有并发的 wbox？）",
            name
        ))
    })?;
    let entry = Entry {
        name: name.to_string(),
        pid: std::process::id(),
        created_unix: now_unix(),
        cmd: cmd.to_vec(),
        target: target.to_string(),
    };
    write_meta_entry(&dir, entry)?;
    Ok(Registration {
        dir,
        lock: Some(lock),
        persist: false,
    })
}

fn write_meta(dir: &Path, name: &str, cmd: &[String], target: &str) -> Result<()> {
    write_meta_entry(
        dir,
        Entry {
            name: name.to_string(),
            pid: std::process::id(),
            created_unix: now_unix(),
            cmd: cmd.to_vec(),
            target: target.to_string(),
        },
    )
}

fn write_meta_entry(dir: &Path, entry: Entry) -> Result<()> {
    std::fs::write(dir.join("meta.json"), entry.to_json())
        .map_err(|e| WboxError::args(format!("写 meta.json 失败：{}", e)))
}

/// 删除一条已退出的容器记录（`wbox rm`）。
///
/// **运行中的一律拒绝**：`rm` 只是删记录，并不会停掉容器；真删了只会让一个
/// 还在跑的容器从 `ps` 里消失，等于把它变成没人管得到的孤儿。要停容器是
/// F8.3 `stop` 的事，两件事不能混。
pub fn remove(name: &str) -> Result<()> {
    let dir = dir_for(name)?;
    if !dir.exists() {
        return Err(WboxError::args(format!("没有名为 '{}' 的容器记录", name)));
    }
    match liveness(&dir) {
        Liveness::Running => Err(WboxError::args(format!(
            "容器 '{}' 仍在运行，拒绝删除记录（rm 不会停掉它）",
            name
        ))),
        Liveness::Exited => {
            purge_dir(&dir);
            if dir.exists() {
                return Err(WboxError::args(format!(
                    "删除状态目录 '{}' 失败（检查权限或是否有进程占用）",
                    dir.display()
                )));
            }
            Ok(())
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 列出所有状态目录。目录不存在时返回空表（还没跑过任何容器）。
///
/// 读不懂的条目直接跳过而不是报错：状态目录是用户可见的普通目录，
/// 手改、残留、跨版本都可能出现，`ps` 不该因为其中一条坏掉就整个失败。
pub fn list() -> Result<Vec<(Entry, Liveness)>> {
    let root = run_root()?;
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let rd = std::fs::read_dir(&root)
        .map_err(|e| WboxError::args(format!("读取 '{}' 失败：{}", root.display(), e)))?;
    for ent in rd.flatten() {
        let dir = ent.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(dir.join("meta.json")) else {
            continue;
        };
        let Some(entry) = Entry::from_json(&text) else {
            continue;
        };
        out.push((entry, liveness(&dir)));
    }
    // 按创建时间排序，最新的在后——与 `docker ps` 相反，但连续观察时更顺眼
    out.sort_by_key(|(e, _)| e.created_unix);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::EnvGuard;

    fn tmp_home(tag: &str) -> (PathBuf, EnvGuard) {
        let d = std::env::temp_dir().join(format!("wbox-rs-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut g = EnvGuard::new();
        g.set("HOME", d.to_str().unwrap());
        g.set("USERPROFILE", d.to_str().unwrap());
        (d, g)
    }

    /// 锁的语义就是存活判定的全部依据，两个平台都必须成立：
    /// 持有期间判 Running，释放后判 Exited。
    ///
    /// 这条测试**在 Windows CI 上同样执行**（test-windows job），所以
    /// `share_mode(0)` 那半实现不是"写完就算"，而是真被跑过。
    #[test]
    fn lock_reflects_owner_liveness() {
        let (home, _g) = tmp_home("lock");
        let reg = register("c1", &["/bin/true".into()], "(native)").unwrap();
        let dir = reg.dir().to_path_buf();
        assert_eq!(liveness(&dir), Liveness::Running, "持锁期间应判为运行中");
        drop(reg);
        // drop 会删掉整个目录，故这里手工造一个"崩溃残留"：有锁文件但无人持有
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lock"), b"").unwrap();
        assert_eq!(liveness(&dir), Liveness::Exited, "无人持锁时应判为已退出");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// 正常退出后状态目录应当消失，不给 `ps` 留垃圾。
    #[test]
    fn registration_cleans_up_on_drop() {
        let (home, _g) = tmp_home("cleanup");
        let dir = {
            let reg = register("c2", &["/bin/true".into()], "(native)").unwrap();
            reg.dir().to_path_buf()
        };
        assert!(!dir.exists(), "drop 后状态目录应已删除：{}", dir.display());
        assert!(list().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// 同名且存活 → 必须报错，不能覆盖（PRD F8.c）。
    #[test]
    fn duplicate_running_name_is_rejected() {
        let (home, _g) = tmp_home("dup");
        let _held = register("dup", &["/bin/true".into()], "(native)").unwrap();
        let err = register("dup", &["/bin/true".into()], "(native)").unwrap_err();
        let m = format!("{}", err);
        assert!(m.contains("正在使用中"), "{}", m);
        // 解法必须是**真能用的**那条：此时 `wbox rm` 会拒绝（容器还在跑），
        // 所以文案不该提它。
        assert!(m.contains("换个 --name"), "报错要给出可行解法：{}", m);
        assert!(!m.contains("wbox rm"), "不该建议一条必然被拒的命令：{}", m);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// 已亡的残留可以被同名容器复用——否则崩过一次就再也用不了这个名字。
    #[test]
    fn stale_entry_is_reused() {
        let (home, _g) = tmp_home("stale");
        let dir = dir_for("s1").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lock"), b"").unwrap();
        std::fs::write(
            dir.join("meta.json"),
            r#"{"name":"s1","pid":1,"created_unix":1,"cmd":["x"],"target":"t"}"#,
        )
        .unwrap();
        assert_eq!(liveness(&dir), Liveness::Exited);
        let reg = register("s1", &["/bin/true".into()], "(native)").unwrap();
        assert_eq!(liveness(reg.dir()), Liveness::Running);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `list` 要能跳过坏条目而不是整个失败——状态目录是用户可见的普通目录。
    #[test]
    fn list_skips_unreadable_entries() {
        let (home, _g) = tmp_home("bad");
        let root = run_root().unwrap();
        std::fs::create_dir_all(root.join("broken")).unwrap();
        std::fs::write(root.join("broken/meta.json"), "{ not json").unwrap();
        std::fs::create_dir_all(root.join("nometa")).unwrap();
        let _ok = register("good", &["/bin/true".into()], "(native)").unwrap();
        let got = list().unwrap();
        assert_eq!(got.len(), 1, "只应列出可读的那条：{:?}", got);
        assert_eq!(got[0].0.name, "good");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// 容器名不得逃出状态根目录。
    #[test]
    fn name_cannot_escape_run_root() {
        let (home, _g) = tmp_home("escape");
        for bad in ["../evil", "a/b", "..", r"a\b"] {
            assert!(dir_for(bad).is_err(), "'{}' 应被拒绝", bad);
        }
        assert!(dir_for("ok-name_1").is_ok());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn entry_json_roundtrip() {
        let e = Entry {
            name: "n".into(),
            pid: 42,
            created_unix: 1700000000,
            cmd: vec!["/bin/sh".into(), "-c".into(), "echo hi".into()],
            target: "alpine:latest".into(),
        };
        assert_eq!(Entry::from_json(&e.to_json()).unwrap(), e);
        assert!(Entry::from_json("{}").is_none());
        assert!(Entry::from_json("nonsense").is_none());
    }
}

#[cfg(test)]
mod cap_tests {
    use super::*;
    use crate::testenv::EnvGuard;

    /// 上限逻辑本身：超限即清空并留下可见标记。
    #[test]
    fn cap_truncates_and_marks() {
        let d = std::env::temp_dir().join(format!("wbox-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut g = EnvGuard::new();
        g.set("WBOX_LOG_MAX_BYTES", "100");
        std::fs::write(d.join(LOG_STDOUT), vec![b'x'; 5000]).unwrap();
        enforce_log_cap(&d, LOG_STDOUT);
        let after = std::fs::read_to_string(d.join(LOG_STDOUT)).unwrap();
        assert!(after.len() < 500, "应已截断，实际 {} 字节", after.len());
        assert!(after.contains("已丢弃此前内容"), "截断要可见：{}", after);
        // 未超限的不动
        std::fs::write(d.join(LOG_STDERR), b"small").unwrap();
        enforce_log_cap(&d, LOG_STDERR);
        assert_eq!(std::fs::read_to_string(d.join(LOG_STDERR)).unwrap(), "small");
        let _ = std::fs::remove_dir_all(&d);
    }
}

// ---------------------------------------------------------------------------
// 进程终止（`wbox stop`，PRD F8.3）
// ---------------------------------------------------------------------------

/// 终止方式。Linux 上先礼后兵，Windows 只有"兵"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kill {
    /// 请求对方自行退出（Linux: SIGTERM）。Windows 无等价物，见 [`terminate_pid`]。
    Graceful,
    /// 强制终止（Linux: SIGKILL；Windows: TerminateProcess）
    Forceful,
}

/// 终止指定进程。返回是否成功递送（**不代表对方已经退出**，退出要靠轮询锁判定）。
///
/// 杀的是 **supervisor（wbox 自己）**，不是 guest：容器整棵进程树的存活绑在
/// supervisor 上（Linux 的 PDEATHSIG、Windows 的 Job kill-on-close），
/// supervisor 一死内核就会收走整棵树。直接去杀 guest 反而漏掉它的子孙。
///
/// **平台差异如实记录**：Windows 没有 SIGTERM 这类"请你自己退出"的通用机制
/// （控制台事件对无控制台的后台进程不适用），因此 `Graceful` 在 Windows 上
/// 等同于 `Forceful`。这不是偷懒，是平台确实没有对齐物；`wbox stop` 的文案
/// 会说明这一点。
#[cfg(unix)]
pub fn terminate_pid(pid: u32, how: Kill) -> bool {
    let sig = match how {
        Kill::Graceful => libc::SIGTERM,
        Kill::Forceful => libc::SIGKILL,
    };
    // SAFETY: kill(2) 只按 pid 递送信号，不解引用任何指针。
    unsafe { libc::kill(pid as libc::pid_t, sig) == 0 }
}

#[cfg(windows)]
pub fn terminate_pid(pid: u32, _how: Kill) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    // SAFETY: OpenProcess 失败返回 null，此时不调用 TerminateProcess；
    // 句柄无论成败都只关一次。
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if h.is_null() {
            return false;
        }
        let ok = TerminateProcess(h, 1) != 0;
        CloseHandle(h);
        ok
    }
}

#[cfg(test)]
mod kill_tests {
    use super::*;

    /// 起一个真的会赖着不走的子进程，用来验证终止确实生效。
    fn spawn_sleeper() -> std::process::Child {
        #[cfg(unix)]
        let mut c = std::process::Command::new("/bin/sleep");
        #[cfg(unix)]
        c.arg("30");
        #[cfg(windows)]
        let mut c = std::process::Command::new("cmd");
        #[cfg(windows)]
        c.args(["/c", "ping -n 30 127.0.0.1 >nul"]);
        c.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleeper")
    }

    /// 终止必须真的把进程干掉。**两个平台都跑这条**——Windows 那半
    /// （OpenProcess + TerminateProcess）我在本机无从验证，靠 CI 的 windows
    /// runner 真实执行；本轮之前正是这种"写完就算"的平台代码出过 Drop 顺序的错。
    #[test]
    fn terminate_actually_kills() {
        let mut child = spawn_sleeper();
        assert!(child.try_wait().unwrap().is_none(), "子进程本应还活着");
        assert!(terminate_pid(child.id(), Kill::Forceful), "递送终止失败");
        // 给内核一点时间收尸
        let mut gone = false;
        for _ in 0..100 {
            if child.try_wait().unwrap().is_some() {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(gone, "终止后子进程仍未退出");
    }

    /// 不存在的 pid 应当返回失败而不是 panic，也不该误伤别人。
    #[test]
    fn terminate_missing_pid_reports_failure() {
        // 极大的 pid 在两个平台都几乎不可能被占用
        assert!(!terminate_pid(4_000_000_000, Kill::Forceful));
    }
}
