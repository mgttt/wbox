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

/// 一次运行的登记。**RAII**：drop 时删掉整个状态目录。
///
/// 崩溃时 drop 不会执行，状态目录会残留——这是刻意接受的：锁已随进程消失，
/// [`liveness`] 会把它判为 `Exited`，`ps` 如实显示，`rm` 负责清理。
/// 与其为"保证清理"引入守护进程，不如让残留可见且可解释。
#[derive(Debug)]
pub struct Registration {
    dir: PathBuf,
    /// 持有到 drop 为止；这个句柄就是"我还活着"的证据
    lock: Option<File>,
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

impl Drop for Registration {
    fn drop(&mut self) {
        // Windows 不允许删除仍被打开的锁文件，因此必须先显式关闭句柄。
        // 任何一步失败都不 panic——清理失败顶多留下一条 Exited 记录。
        drop(self.lock.take());
        let _ = std::fs::remove_file(self.dir.join("meta.json"));
        let _ = std::fs::remove_file(self.dir.join("lock"));
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// 登记一次运行。同名容器已在运行时**报错**而不是覆盖——覆盖会让用户
/// "以为还在跑的那个"悄无声息地从列表里消失（PRD F8.c）。
pub fn register(name: &str, cmd: &[String], target: &str) -> Result<Registration> {
    let dir = dir_for(name)?;
    if dir.exists() {
        match liveness(&dir) {
            Liveness::Running => {
                return Err(WboxError::args(format!(
                    "容器名 '{}' 正在使用中。换个 --name，或先 `wbox rm {}`",
                    name, name
                )))
            }
            // 已亡的残留：可以安全复用，直接清掉重来
            Liveness::Exited => {
                let _ = std::fs::remove_file(dir.join("meta.json"));
                let _ = std::fs::remove_file(dir.join("lock"));
                let _ = std::fs::remove_dir(&dir);
            }
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
    std::fs::write(dir.join("meta.json"), entry.to_json())
        .map_err(|e| WboxError::args(format!("写 meta.json 失败：{}", e)))?;
    Ok(Registration {
        dir,
        lock: Some(lock),
    })
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
        assert!(m.contains("wbox rm"), "报错要给出解法：{}", m);
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
